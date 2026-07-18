use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use axum_extra::extract::{
    CookieJar,
    cookie::{Cookie, SameSite},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use time::Duration as CookieDuration;
use traceable_search::{
    ChatResearchAnswerResponse, ClarificationEvent, ClarificationEventKind, ClarificationState,
    ClarificationStatus, DialogueMessage, ExplorationStopReason, ModelAccessConfig,
    OpenAiCompatibleModelClient, RationaleAuditStatus, ResearchAnswerResponse, ResearchAnswerStyle,
    SearchBoundaryContractFailure, SearchEngine, SearchEngineAttemptOutcome,
    SearchEngineUnavailability, TraceEvent, TraceEventEnvelope, TracePolicy,
    project_chat_research_answer, replay_trace, validate_public_web_url,
};
use url::Url;
use uuid::Uuid;

use crate::{
    DemoHostState, PublicHttpError,
    catalog::{
        CatalogError, DEFAULT_RESEARCH_CONVERSATION_TITLE, ModelProfileRecord, NewModelProfile,
        NewResearchConversation, NewResearchTurn, ResearchConversationRecord, ResearchTurnRecord,
        ResearchTurnStatus, UpdatedModelProfile, UserAccountRecord,
    },
    security::{generate_login_token, hash_login_token, hash_password, password_matches},
};

const LOGIN_COOKIE_NAME: &str = "traceable_login";
const LOGIN_SESSION_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_EMAIL_CHARS: usize = 320;
const MIN_PASSWORD_CHARS: usize = 12;
const MAX_PASSWORD_CHARS: usize = 200;
const MAX_DISPLAY_NAME_CHARS: usize = 80;
const MAX_PROFILE_NAME_CHARS: usize = 80;
const MAX_MODEL_ID_CHARS: usize = 200;
const MAX_API_BASE_URL_CHARS: usize = 2_048;
const MAX_CONVERSATION_TITLE_CHARS: usize = 200;
const AUTOMATIC_RESEARCH_FAILURE_MESSAGE: &str = "研究未能自动完成。请直接发送新的研究问题。";
const AUTOMATIC_RESEARCH_FAILURE_SUMMARY: &str = "Automatic research could not complete.";

pub fn routes() -> Router<Arc<DemoHostState>> {
    Router::new()
        .route("/auth/register", post(register_account))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(current_account))
        .route(
            "/model-profiles",
            get(list_model_profiles).post(create_model_profile),
        )
        .route(
            "/model-profiles/{profile_id}",
            patch(update_model_profile).delete(archive_model_profile),
        )
        .route(
            "/model-profiles/{profile_id}/default",
            post(set_default_model_profile),
        )
        .route(
            "/model-profiles/{profile_id}/verify",
            post(verify_model_profile),
        )
        .route(
            "/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/conversations/{conversation_id}",
            get(load_conversation)
                .patch(update_conversation)
                .delete(archive_conversation),
        )
        .route(
            "/conversations/{conversation_id}/turns",
            post(create_dialogue_turn),
        )
        .route(
            "/conversations/{conversation_id}/turns/{turn_id}/messages",
            post(submit_dialogue_message),
        )
        .route(
            "/conversations/{conversation_id}/turns/{turn_id}/trace/summary",
            get(load_research_turn_trace_summary),
        )
        .route(
            "/conversations/{conversation_id}/turns/{turn_id}/trace/audit",
            get(load_research_turn_trace_audit),
        )
}

#[derive(Debug, Deserialize)]
struct RegisterAccountRequest {
    email: String,
    password: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct UserAccountResponse {
    user_id: String,
    email: String,
    display_name: String,
    created_at: i64,
}

#[derive(Debug, Deserialize)]
struct CreateModelProfileRequest {
    display_name: String,
    api_base_url: String,
    api_key: String,
    model_id: String,
    #[serde(default)]
    make_default: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateModelProfileRequest {
    display_name: Option<String>,
    api_base_url: Option<String>,
    api_key: Option<String>,
    model_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ModelProfileResponse {
    profile_id: String,
    display_name: String,
    api_base_url: String,
    model_id: String,
    revision: i64,
    is_default: bool,
    has_api_key: bool,
    verified_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Deserialize)]
struct CreateConversationRequest {
    title: Option<String>,
    model_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateConversationRequest {
    title: Option<String>,
    model_profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConversationSummaryResponse {
    conversation_id: String,
    title: String,
    model_profile_id: String,
    model_profile_name: String,
    turn_count: i64,
    latest_turn_status: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct ConversationDetailResponse {
    #[serde(flatten)]
    conversation: ConversationSummaryResponse,
    turns: Vec<ResearchTurnResponse>,
}

#[derive(Debug, Deserialize)]
/// The first normal user message in a research turn. It begins model-led
/// dialogue; it is not an instruction to manually execute research.
struct CreateDialogueTurnRequest {
    question: String,
    #[serde(default)]
    answer_style: ResearchAnswerStyle,
}

#[derive(Debug, Deserialize)]
struct DialogueMessageRequest {
    revision: u32,
    message: String,
}

#[derive(Debug, Serialize)]
struct TurnDialogueResponse {
    revision: u32,
    status: &'static str,
    messages: Vec<DialogueMessage>,
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResearchTurnResponse {
    turn_id: String,
    turn_number: i64,
    user_question: String,
    status: &'static str,
    answer: Option<ChatResearchAnswerResponse>,
    dialogue: Option<TurnDialogueResponse>,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ResearchTraceSummaryResponse {
    run_id: Option<String>,
    clarification_rationale_audit_status: RationaleAuditStatus,
    research_rationale_audit_status: Option<RationaleAuditStatus>,
    understanding: Option<TraceUnderstandingResponse>,
    rounds: Vec<TraceRoundResponse>,
    archived_source_count: usize,
    skipped_source_count: usize,
    selected_sources: Vec<TraceSourceResponse>,
    synthesis_rationale: Option<String>,
    failure: Option<TraceFailureResponse>,
}

#[derive(Debug, Serialize)]
struct TraceUnderstandingResponse {
    message: String,
    rationale: String,
}

#[derive(Debug, Serialize)]
struct TraceRoundResponse {
    round: u32,
    directions: Vec<String>,
    search_result_count: usize,
}

#[derive(Debug, Serialize)]
struct TraceSourceResponse {
    title: String,
    url: String,
    rationale: String,
}

#[derive(Debug, Serialize)]
struct TraceFailureResponse {
    stage: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct TraceAuditQuery {
    stage: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct TraceAuditPageResponse {
    run_id: Option<String>,
    next_cursor: Option<usize>,
    entries: Vec<TraceAuditEntryResponse>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceAuditEntryResponse {
    sequence: Option<u64>,
    occurred_at: Option<DateTime<Utc>>,
    stage: &'static str,
    label: &'static str,
    detail: String,
    rationale: Option<String>,
}

struct LoadedTurnTrace {
    turn: ResearchTurnRecord,
    clarification: ClarificationState,
    clarification_events: Vec<ClarificationEvent>,
    research_events: Vec<TraceEventEnvelope>,
    research_rationale_audit_status: Option<RationaleAuditStatus>,
}

async fn register_account(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Json(request): Json<RegisterAccountRequest>,
) -> Result<(CookieJar, Json<UserAccountResponse>), PublicHttpError> {
    let normalized_email = normalize_email(&request.email)?;
    let display_name = validate_trimmed_text(
        &request.display_name,
        1,
        MAX_DISPLAY_NAME_CHARS,
        "invalid_display_name",
        "显示名称长度无效",
    )?;
    validate_password(&request.password)?;
    let password = request.password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(PublicHttpError::internal_failure)?;
    let now = now();
    let account = state
        .catalog
        .create_user_account(
            &new_public_id(),
            &normalized_email,
            &display_name,
            &password_hash,
            now,
        )
        .map_err(map_catalog_error)?;
    let (jar, _) = create_login_session(&state, jar, &account.user_id, now)?;
    Ok((jar, Json(project_user_account(account))))
}

async fn login(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Json(request): Json<LoginRequest>,
) -> Result<(CookieJar, Json<UserAccountResponse>), PublicHttpError> {
    let normalized_email = normalize_email(&request.email)?;
    if request.password.chars().count() > MAX_PASSWORD_CHARS {
        return Err(invalid_credentials());
    }
    let account = state
        .catalog
        .user_account_by_email(&normalized_email)
        .map_err(map_catalog_error)?;
    let Some(account) = account else {
        let supplied_password = request.password;
        let _ = tokio::task::spawn_blocking(move || {
            let dummy_hash = hash_password("fixed dummy password for timing equalization");
            password_matches(&supplied_password, &dummy_hash)
        })
        .await;
        return Err(invalid_credentials());
    };
    let supplied_password = request.password;
    let password_hash = account.password_hash.clone();
    let matches =
        tokio::task::spawn_blocking(move || password_matches(&supplied_password, &password_hash))
            .await
            .map_err(PublicHttpError::internal_failure)?;
    if !matches {
        return Err(invalid_credentials());
    }
    let now = now();
    let (jar, _) = create_login_session(&state, jar, &account.user_id, now)?;
    Ok((jar, Json(project_user_account(account))))
}

async fn logout(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), PublicHttpError> {
    if let Some(cookie) = jar.get(LOGIN_COOKIE_NAME) {
        state
            .catalog
            .revoke_login_session(&hash_login_token(cookie.value()), now())
            .map_err(map_catalog_error)?;
    }
    Ok((
        jar.remove(removal_cookie(state.secure_cookies)),
        StatusCode::NO_CONTENT,
    ))
}

async fn current_account(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<Json<UserAccountResponse>, PublicHttpError> {
    Ok(Json(project_user_account(authenticated_user(
        &state, &jar,
    )?)))
}

async fn list_model_profiles(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<Json<Vec<ModelProfileResponse>>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let profiles = state
        .catalog
        .list_model_profiles(&user.user_id)
        .map_err(map_catalog_error)?
        .into_iter()
        .map(project_model_profile)
        .collect();
    Ok(Json(profiles))
}

async fn create_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Json(request): Json<CreateModelProfileRequest>,
) -> Result<Json<ModelProfileResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let display_name = validate_trimmed_text(
        &request.display_name,
        1,
        MAX_PROFILE_NAME_CHARS,
        "invalid_profile_name",
        "模型配置名称长度无效",
    )?;
    let model_id = validate_trimmed_text(
        &request.model_id,
        1,
        MAX_MODEL_ID_CHARS,
        "invalid_model_id",
        "模型 ID 长度无效",
    )?;
    let api_base_url =
        validate_model_access(&state, &request.api_base_url, &request.api_key, &model_id).await?;
    let profile_id = new_public_id();
    let encrypted = state
        .credential_cipher
        .encrypt(&user.user_id, &profile_id, &request.api_key)
        .map_err(PublicHttpError::internal_failure)?;
    let profile = state
        .catalog
        .create_model_profile(NewModelProfile {
            profile_id: &profile_id,
            user_id: &user.user_id,
            display_name: &display_name,
            api_base_url: &api_base_url,
            model_id: &model_id,
            encrypted_api_key: &encrypted,
            make_default: request.make_default,
            now: now(),
        })
        .map_err(map_catalog_error)?;
    Ok(Json(project_model_profile(profile)))
}

async fn update_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
    Json(request): Json<UpdateModelProfileRequest>,
) -> Result<Json<ModelProfileResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let existing = state
        .catalog
        .model_profile(&user.user_id, &profile_id)
        .map_err(map_catalog_error)?;
    let display_name = match request.display_name {
        Some(value) => validate_trimmed_text(
            &value,
            1,
            MAX_PROFILE_NAME_CHARS,
            "invalid_profile_name",
            "模型配置名称长度无效",
        )?,
        None => existing.display_name.clone(),
    };
    let model_id = match request.model_id {
        Some(value) => validate_trimmed_text(
            &value,
            1,
            MAX_MODEL_ID_CHARS,
            "invalid_model_id",
            "模型 ID 长度无效",
        )?,
        None => existing.model_id.clone(),
    };
    let api_key = match request.api_key {
        Some(value) => value,
        None => decrypt_profile_api_key(&state, &existing)?,
    };
    let requested_base_url = request
        .api_base_url
        .as_deref()
        .unwrap_or(&existing.api_base_url);
    let api_base_url =
        validate_model_access(&state, requested_base_url, &api_key, &model_id).await?;
    let encrypted = state
        .credential_cipher
        .encrypt(&user.user_id, &profile_id, &api_key)
        .map_err(PublicHttpError::internal_failure)?;
    let updated = state
        .catalog
        .update_model_profile(UpdatedModelProfile {
            profile_id: &profile_id,
            user_id: &user.user_id,
            display_name: &display_name,
            api_base_url: &api_base_url,
            model_id: &model_id,
            encrypted_api_key: &encrypted,
            now: now(),
        })
        .map_err(map_catalog_error)?;
    Ok(Json(project_model_profile(updated)))
}

async fn set_default_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    state
        .catalog
        .set_default_model_profile(&user.user_id, &profile_id, now())
        .map_err(map_catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn verify_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let profile = state
        .catalog
        .model_profile(&user.user_id, &profile_id)
        .map_err(map_catalog_error)?;
    let api_key = decrypt_profile_api_key(&state, &profile)?;
    let client =
        OpenAiCompatibleModelClient::new(&profile.api_base_url, api_key, &profile.model_id)
            .map_err(|_| model_verification_failed())?;
    client
        .generate_structured_output::<serde_json::Value>("Return JSON only.", r#"{"ok":true}"#)
        .await
        .map_err(|_| model_verification_failed())?;
    state
        .catalog
        .mark_model_profile_verified(&user.user_id, &profile_id, now())
        .map_err(map_catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn archive_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    state
        .catalog
        .archive_model_profile(&user.user_id, &profile_id, now())
        .map_err(map_catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_conversations(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<Json<Vec<ConversationSummaryResponse>>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let conversations = state
        .catalog
        .list_research_conversations(&user.user_id)
        .map_err(map_catalog_error)?
        .into_iter()
        .map(project_conversation_summary)
        .collect();
    Ok(Json(conversations))
}

async fn create_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Json(request): Json<CreateConversationRequest>,
) -> Result<Json<ConversationDetailResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let profile = match request.model_profile_id {
        Some(profile_id) => state
            .catalog
            .model_profile(&user.user_id, &profile_id)
            .map_err(map_catalog_error)?,
        None => {
            state
                .catalog
                .default_model_profile(&user.user_id)
                .map_err(|error| match error {
                    CatalogError::NotFound => {
                        PublicHttpError::conflict("model_profile_required", "请先添加模型配置")
                    }
                    other => map_catalog_error(other),
                })?
        }
    };
    let title = match request.title {
        Some(value) => validate_trimmed_text(
            &value,
            1,
            MAX_CONVERSATION_TITLE_CHARS,
            "invalid_conversation_title",
            "会话标题长度无效",
        )?,
        None => DEFAULT_RESEARCH_CONVERSATION_TITLE.into(),
    };
    let core_conversation = state
        .research_runtime
        .create_conversation()
        .await
        .map_err(PublicHttpError::internal_failure)?;
    let conversation = state
        .catalog
        .create_research_conversation(NewResearchConversation {
            conversation_id: &new_public_id(),
            user_id: &user.user_id,
            core_conversation_id: &core_conversation.session_id,
            title: &title,
            model_profile_id: &profile.profile_id,
            now: now(),
        })
        .map_err(map_catalog_error)?;
    Ok(Json(build_conversation_detail(&state, conversation).await?))
}

async fn load_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
) -> Result<Json<ConversationDetailResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let conversation = state
        .catalog
        .research_conversation(&user.user_id, &conversation_id)
        .map_err(map_catalog_error)?;
    Ok(Json(build_conversation_detail(&state, conversation).await?))
}

async fn update_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
    Json(request): Json<UpdateConversationRequest>,
) -> Result<Json<ConversationSummaryResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let existing = state
        .catalog
        .research_conversation(&user.user_id, &conversation_id)
        .map_err(map_catalog_error)?;
    let title = match request.title {
        Some(value) => validate_trimmed_text(
            &value,
            1,
            MAX_CONVERSATION_TITLE_CHARS,
            "invalid_conversation_title",
            "会话标题长度无效",
        )?,
        None => existing.title,
    };
    let model_profile_id = request
        .model_profile_id
        .unwrap_or(existing.model_profile_id);
    let updated = state
        .catalog
        .update_research_conversation(
            &user.user_id,
            &conversation_id,
            &title,
            &model_profile_id,
            now(),
        )
        .map_err(map_catalog_error)?;
    Ok(Json(project_conversation_summary(updated)))
}

async fn archive_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
) -> Result<StatusCode, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    state
        .catalog
        .archive_research_conversation(&user.user_id, &conversation_id, now())
        .map_err(map_catalog_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_dialogue_turn(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
    Json(request): Json<CreateDialogueTurnRequest>,
) -> Result<Json<ResearchTurnResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let question = validate_trimmed_text(
        &request.question,
        1,
        4_000,
        "invalid_question",
        "研究问题长度无效",
    )?;
    let conversation = state
        .catalog
        .research_conversation(&user.user_id, &conversation_id)
        .map_err(map_catalog_error)?;
    let profile = state
        .catalog
        .model_profile(&user.user_id, &conversation.model_profile_id)
        .map_err(map_catalog_error)?;
    let model_access = model_access_from_profile(&state, &profile)?;
    let clarification = state
        .research_runtime
        .start_research_turn(&conversation.core_conversation_id, &question, &model_access)
        .await
        .map_err(PublicHttpError::internal_failure)?;
    // The model request above awaits external I/O. Re-check the pinned profile
    // before recording the turn so a concurrent profile edit cannot create a
    // dialogue that later fails its revision guard.
    let current_conversation = match state
        .catalog
        .research_conversation(&user.user_id, &conversation_id)
    {
        Ok(current_conversation) => current_conversation,
        Err(error) => {
            if let Err(cancel_error) = state
                .research_runtime
                .cancel_clarification(&clarification.clarification_id, clarification.revision)
                .await
            {
                tracing::error!(error = %cancel_error, "failed to compensate clarification after conversation lookup failed");
            }
            return Err(map_catalog_error(error));
        }
    };
    if current_conversation.model_profile_id != conversation.model_profile_id {
        state
            .research_runtime
            .cancel_clarification(&clarification.clarification_id, clarification.revision)
            .await
            .map_err(PublicHttpError::internal_failure)?;
        return Err(PublicHttpError::conflict(
            "conversation_model_profile_changed",
            "会话模型配置已变更，请重新发送研究问题",
        ));
    }
    let current_profile = match state
        .catalog
        .model_profile(&user.user_id, &conversation.model_profile_id)
    {
        Ok(current_profile) => current_profile,
        Err(error) => {
            if let Err(cancel_error) = state
                .research_runtime
                .cancel_clarification(&clarification.clarification_id, clarification.revision)
                .await
            {
                tracing::error!(error = %cancel_error, "failed to compensate clarification after model-profile lookup failed");
            }
            return Err(map_catalog_error(error));
        }
    };
    if current_profile.revision != profile.revision {
        state
            .research_runtime
            .cancel_clarification(&clarification.clarification_id, clarification.revision)
            .await
            .map_err(PublicHttpError::internal_failure)?;
        return Err(PublicHttpError::conflict(
            "model_profile_changed",
            "模型配置已变更，请重新发送研究问题",
        ));
    }
    let turn_id = new_public_id();
    let status = clarification_catalog_status(&clarification);
    let created = state.catalog.create_research_turn(NewResearchTurn {
        turn_id: &turn_id,
        conversation_id: &conversation_id,
        turn_number: i64::try_from(clarification.turn.unwrap_or_default())
            .map_err(PublicHttpError::internal_failure)?,
        clarification_id: &clarification.clarification_id,
        user_question: &question,
        status,
        answer_style: request.answer_style,
        model_profile: &profile,
        now: now(),
    });
    let turn = match created {
        Ok(turn) => turn,
        Err(error) => {
            if let Err(cancel_error) = state
                .research_runtime
                .cancel_clarification(&clarification.clarification_id, clarification.revision)
                .await
            {
                tracing::error!(error = %cancel_error, "failed to compensate unregistered clarification");
            }
            return Err(map_catalog_error(error));
        }
    };
    let (turn, clarification) = schedule_automatic_research_turn(
        &state,
        user.user_id.clone(),
        conversation_id.clone(),
        turn,
        clarification,
        model_access,
    );
    Ok(Json(project_research_turn(turn, Some(clarification))?))
}

async fn submit_dialogue_message(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path((conversation_id, turn_id)): Path<(String, String)>,
    Json(request): Json<DialogueMessageRequest>,
) -> Result<Json<ResearchTurnResponse>, PublicHttpError> {
    let message = validate_trimmed_text(
        &request.message,
        1,
        4_000,
        "invalid_dialogue_message",
        "消息长度无效",
    )?;
    let (user, turn, model_access) =
        owned_turn_with_model(&state, &jar, &conversation_id, &turn_id)?;
    let clarification = state
        .research_runtime
        .submit_dialogue_message(
            &turn.clarification_id,
            request.revision,
            &message,
            &model_access,
        )
        .await
        .map_err(PublicHttpError::internal_failure)?;
    state
        .catalog
        .update_research_turn_status(
            &turn.turn_id,
            clarification_catalog_status(&clarification),
            None,
            None,
            now(),
        )
        .map_err(map_catalog_error)?;
    let updated = state
        .catalog
        .owned_research_turn(&user.user_id, &conversation_id, &turn_id)
        .map_err(map_catalog_error)?;
    let (updated, clarification) = schedule_automatic_research_turn(
        &state,
        user.user_id.clone(),
        conversation_id.clone(),
        updated,
        clarification,
        model_access,
    );
    Ok(Json(project_research_turn(updated, Some(clarification))?))
}

/// Research starts as a consequence of the model's `start_research` decision,
/// never as a second browser command. Scheduling returns the pending Turn to
/// the browser immediately; a detached task owns execution and terminalization.
fn schedule_automatic_research_turn(
    state: &Arc<DemoHostState>,
    user_id: String,
    conversation_id: String,
    turn: ResearchTurnRecord,
    clarification: ClarificationState,
    model_access: ModelAccessConfig,
) -> (ResearchTurnRecord, ClarificationState) {
    if !is_automatic_execution_pending(turn.status, clarification.status) {
        return (turn, clarification);
    }
    let execution_state = Arc::clone(state);
    let execution_turn = turn.clone();
    tokio::spawn(async move {
        let turn_id = execution_turn.turn_id.clone();
        if let Err(error) = execute_scheduled_research_turn(
            &execution_state,
            &user_id,
            &conversation_id,
            execution_turn,
            &model_access,
        )
        .await
        {
            tracing::error!(turn_id = %turn_id, "scheduled automatic research failed: {}", error.public_message);
        }
    });
    (turn, clarification)
}

async fn execute_scheduled_research_turn(
    state: &Arc<DemoHostState>,
    user_id: &str,
    conversation_id: &str,
    turn: ResearchTurnRecord,
    model_access: &ModelAccessConfig,
) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
    let permit =
        state.research_slots.acquire().await.map_err(|_| {
            PublicHttpError::internal_failure("research capacity semaphore was closed")
        })?;
    let prepared = match state
        .research_runtime
        .prepare_research_run_with_answer_style(
            &turn.clarification_id,
            TracePolicy::default(),
            turn.answer_style,
        )
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::error!(error = %error, turn_id = %turn.turn_id, "automatic research preparation failed");
            return fail_automatic_research_turn(state, user_id, conversation_id, turn).await;
        }
    };
    state
        .catalog
        .update_research_turn_status(
            &turn.turn_id,
            ResearchTurnStatus::Running,
            Some(&prepared.run_id),
            None,
            now(),
        )
        .map_err(map_catalog_error)?;
    let result = state
        .research_runtime
        .execute_prepared_research(prepared, model_access)
        .await;
    drop(permit);
    match result {
        Ok(answer) => {
            let answer_json =
                serde_json::to_string(&answer).map_err(PublicHttpError::internal_failure)?;
            state
                .catalog
                .update_research_turn_status(
                    &turn.turn_id,
                    ResearchTurnStatus::Completed,
                    None,
                    Some(&answer_json),
                    now(),
                )
                .map_err(map_catalog_error)?;
        }
        Err(error) => {
            tracing::error!(error = %error, turn_id = %turn.turn_id, "automatic research turn failed");
            return fail_automatic_research_turn(state, user_id, conversation_id, turn).await;
        }
    }
    let updated = state
        .catalog
        .owned_research_turn(user_id, conversation_id, &turn.turn_id)
        .map_err(map_catalog_error)?;
    let clarification = state
        .research_runtime
        .load_clarification(&turn.clarification_id)
        .await
        .map_err(PublicHttpError::internal_failure)?;
    Ok((updated, clarification))
}

/// Records an automatic-execution failure as a terminal host state. When the
/// preparation attempt failed before its append-only preparation event, the
/// intake is still `ResearchReady`; terminalizing the preparation failure
/// releases the core conversation without requiring a browser action.
async fn fail_automatic_research_turn(
    state: &Arc<DemoHostState>,
    user_id: &str,
    conversation_id: &str,
    turn: ResearchTurnRecord,
) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
    let current = state
        .research_runtime
        .load_clarification(&turn.clarification_id)
        .await
        .map_err(PublicHttpError::internal_failure)?;
    let clarification = match current.status {
        ClarificationStatus::ResearchReady => state
            .research_runtime
            .terminalize_research_preparation_failure(
                &turn.clarification_id,
                AUTOMATIC_RESEARCH_FAILURE_SUMMARY,
            )
            .await
            .map_err(PublicHttpError::internal_failure)?,
        ClarificationStatus::ResearchPrepared => {
            let preparation = current.preparation.as_ref().ok_or_else(|| {
                PublicHttpError::internal_failure("prepared research has no run identifier")
            })?;
            state
                .research_runtime
                .terminalize_prepared_research_failure(
                    &turn.clarification_id,
                    &preparation.run_id,
                    AUTOMATIC_RESEARCH_FAILURE_SUMMARY,
                )
                .await
                .map_err(PublicHttpError::internal_failure)?
        }
        ClarificationStatus::ResearchFailed | ClarificationStatus::Cancelled => current,
        status => {
            return Err(PublicHttpError::internal_failure(format!(
                "automatic research failure cannot terminalize clarification in {status:?}"
            )));
        }
    };
    let status = match clarification.status {
        ClarificationStatus::ResearchFailed => ResearchTurnStatus::Failed,
        ClarificationStatus::Cancelled => ResearchTurnStatus::Cancelled,
        status => {
            return Err(PublicHttpError::internal_failure(format!(
                "automatic research failure did not produce a terminal clarification state: {status:?}"
            )));
        }
    };
    state
        .catalog
        .update_research_turn_status(&turn.turn_id, status, None, None, now())
        .map_err(map_catalog_error)?;
    let updated = state
        .catalog
        .owned_research_turn(user_id, conversation_id, &turn.turn_id)
        .map_err(map_catalog_error)?;
    Ok((updated, clarification))
}

fn is_automatic_execution_pending(
    turn_status: ResearchTurnStatus,
    clarification_status: ClarificationStatus,
) -> bool {
    matches!(
        turn_status,
        ResearchTurnStatus::Ready | ResearchTurnStatus::Running
    ) && matches!(
        clarification_status,
        ClarificationStatus::ResearchReady | ClarificationStatus::ResearchPrepared
    )
}

/// A server restart can occur after the model committed `start_research` but
/// before execution finished. The startup recovery coordinator resumes that
/// work using the turn's pinned model-profile revision; it never asks the
/// browser to confirm or press an execute command.
async fn resume_automatic_execution_if_needed(
    state: &Arc<DemoHostState>,
    user_id: &str,
    conversation_id: &str,
    turn: ResearchTurnRecord,
    clarification: ClarificationState,
) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
    if matches!(
        clarification.status,
        ClarificationStatus::ResearchFailed | ClarificationStatus::Cancelled
    ) {
        return reconcile_terminal_automatic_turn(
            state,
            user_id,
            conversation_id,
            turn,
            clarification,
        )
        .await;
    }
    if !is_automatic_execution_pending(turn.status, clarification.status) {
        return Ok((turn, clarification));
    }
    let profile = match state.catalog.model_profile_for_turn(user_id, &turn) {
        Ok(profile) => profile,
        Err(error) => {
            tracing::error!(error = %error, turn_id = %turn.turn_id, "automatic research cannot resume with its pinned model profile");
            return fail_automatic_research_turn(state, user_id, conversation_id, turn).await;
        }
    };
    let model_access = match model_access_from_profile(state, &profile) {
        Ok(model_access) => model_access,
        Err(error) => {
            tracing::error!(turn_id = %turn.turn_id, "automatic research cannot resume because model access is unavailable");
            let _ = error;
            return fail_automatic_research_turn(state, user_id, conversation_id, turn).await;
        }
    };
    Ok(schedule_automatic_research_turn(
        state,
        user_id.to_owned(),
        conversation_id.to_owned(),
        turn,
        clarification,
        model_access,
    ))
}

/// Repairs the host catalogue after a crash between the append-only core
/// terminal event and its SQL status update. This deliberately performs no
/// model call or research execution.
async fn reconcile_terminal_automatic_turn(
    state: &Arc<DemoHostState>,
    user_id: &str,
    conversation_id: &str,
    turn: ResearchTurnRecord,
    clarification: ClarificationState,
) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
    let status = match clarification.status {
        ClarificationStatus::ResearchFailed => ResearchTurnStatus::Failed,
        ClarificationStatus::Cancelled => ResearchTurnStatus::Cancelled,
        status => {
            return Err(PublicHttpError::internal_failure(format!(
                "terminal reconciliation received nonterminal clarification state {status:?}"
            )));
        }
    };
    state
        .catalog
        .update_research_turn_status(&turn.turn_id, status, None, None, now())
        .map_err(map_catalog_error)?;
    let updated = state
        .catalog
        .owned_research_turn(user_id, conversation_id, &turn.turn_id)
        .map_err(map_catalog_error)?;
    Ok((updated, clarification))
}

/// Starts bounded, detached recovery after the host has constructed its shared
/// state. This preserves read-only conversation routes while ensuring a crash
/// cannot turn a model-approved `start_research` decision into a browser
/// action. Runtime clarification and conversation locks make duplicate calls
/// idempotent: preparation reuses its persisted run id, and execution recovers
/// a completed or failed trace instead of running it twice.
pub(crate) fn start_automatic_execution_recovery(state: Arc<DemoHostState>) {
    tokio::spawn(async move {
        let candidates = match state.catalog.automatic_execution_recovery_candidates() {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::error!(error = %error, "could not list automatic research recovery candidates");
                return;
            }
        };
        if !candidates.is_empty() {
            tracing::info!(
                count = candidates.len(),
                "recovering automatic research turns"
            );
        }
        for candidate in candidates {
            let turn = candidate.turn;
            let turn_id = turn.turn_id.clone();
            let conversation_id = turn.conversation_id.clone();
            let clarification = match state
                .research_runtime
                .load_clarification(&turn.clarification_id)
                .await
            {
                Ok(clarification) => clarification,
                Err(error) => {
                    tracing::error!(error = %error, turn_id = %turn.turn_id, "could not load automatic research recovery state");
                    continue;
                }
            };
            if let Err(error) = resume_automatic_execution_if_needed(
                &state,
                &candidate.user_id,
                &conversation_id,
                turn,
                clarification,
            )
            .await
            {
                tracing::error!(turn_id = %turn_id, "automatic research recovery failed: {}", error.public_message);
            }
        }
    });
}

async fn load_research_turn_trace_summary(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path((conversation_id, turn_id)): Path<(String, String)>,
) -> Result<Json<ResearchTraceSummaryResponse>, PublicHttpError> {
    let trace = load_owned_turn_trace(&state, &jar, &conversation_id, &turn_id).await?;
    Ok(Json(project_trace_summary(&trace)))
}

async fn load_research_turn_trace_audit(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path((conversation_id, turn_id)): Path<(String, String)>,
    Query(query): Query<TraceAuditQuery>,
) -> Result<Json<TraceAuditPageResponse>, PublicHttpError> {
    let stage_filter = query
        .stage
        .as_deref()
        .map(validate_audit_stage)
        .transpose()?;
    let trace = load_owned_turn_trace(&state, &jar, &conversation_id, &turn_id).await?;
    let entries = project_audit_entries(&trace);
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|entry| stage_filter.is_none_or(|stage| entry.stage == stage))
        .collect();
    let cursor = query.cursor.unwrap_or(0);
    if cursor > filtered.len() {
        return Err(PublicHttpError::invalid_request());
    }
    let limit = query.limit.unwrap_or(40).clamp(1, 100);
    let end = cursor.saturating_add(limit).min(filtered.len());
    Ok(Json(TraceAuditPageResponse {
        run_id: trace.turn.run_id,
        next_cursor: (end < filtered.len()).then_some(end),
        entries: filtered[cursor..end].to_vec(),
    }))
}

async fn load_owned_turn_trace(
    state: &Arc<DemoHostState>,
    jar: &CookieJar,
    conversation_id: &str,
    turn_id: &str,
) -> Result<LoadedTurnTrace, PublicHttpError> {
    let user = authenticated_user(state, jar)?;
    let turn = state
        .catalog
        .owned_research_turn(&user.user_id, conversation_id, turn_id)
        .map_err(map_catalog_error)?;
    let clarification = state
        .research_runtime
        .load_clarification(&turn.clarification_id)
        .await
        .map_err(|error| PublicHttpError::internal_failure(error.to_string()))?;
    let clarification_events = read_jsonl_events::<ClarificationEvent>(
        state
            .research_runtime
            .clarification_trace_path(&turn.clarification_id),
    )?;
    let (research_events, research_rationale_audit_status) = if let Some(run_id) = &turn.run_id {
        let replayed = replay_trace(state.research_runtime.research_trace_path(run_id))
            .map_err(|error| PublicHttpError::internal_failure(error.to_string()))?;
        if replayed.header.run_id != *run_id
            || replayed.header.clarification_id != turn.clarification_id
        {
            return Err(PublicHttpError::internal_failure(
                "research trace header does not match the owned turn",
            ));
        }
        (
            replayed.events,
            Some(RationaleAuditStatus::RequiredAndValidated),
        )
    } else {
        (Vec::new(), None)
    };
    Ok(LoadedTurnTrace {
        turn,
        clarification,
        clarification_events,
        research_events,
        research_rationale_audit_status,
    })
}

fn project_trace_summary(trace: &LoadedTurnTrace) -> ResearchTraceSummaryResponse {
    let understanding = trace.clarification_events.iter().rev().find_map(|event| {
        if let ClarificationEventKind::ModelUnderstanding {
            assistant_message,
            rationale,
            ..
        } = &event.kind
        {
            Some(TraceUnderstandingResponse {
                message: concise_trace_text(assistant_message, 320),
                rationale: concise_trace_text(rationale, 320),
            })
        } else {
            None
        }
    });
    let mut rounds = BTreeMap::<u32, TraceRoundResponse>::new();
    let mut snapshot_titles = HashMap::<String, String>::new();
    let mut snapshot_urls = HashMap::<String, String>::new();
    let mut archived_source_count = 0;
    let mut skipped_source_count = 0;
    let mut synthesis_rationale = None;
    let mut failure = trace.clarification_events.iter().rev().find_map(|event| {
        let (stage, message) = match &event.kind {
            ClarificationEventKind::ResearchPreparationFailed { message, .. } => {
                ("preparation", message)
            }
            ClarificationEventKind::ResearchRunFailed { message, .. } => {
                ("initialization", message)
            }
            _ => return None,
        };
        Some(TraceFailureResponse {
            stage: stage.into(),
            message: concise_trace_text(message, 320),
        })
    });
    for envelope in &trace.research_events {
        match &envelope.event {
            TraceEvent::SearchQuery { round, query, gap } => {
                let entry = rounds.entry(*round).or_insert_with(|| TraceRoundResponse {
                    round: *round,
                    directions: Vec::new(),
                    search_result_count: 0,
                });
                if entry.directions.len() < 3 {
                    entry.directions.push(format!(
                        "{}：{}",
                        concise_trace_text(query, 96),
                        concise_trace_text(gap, 160)
                    ));
                }
            }
            TraceEvent::SearchResult { round, .. } => {
                let entry = rounds.entry(*round).or_insert_with(|| TraceRoundResponse {
                    round: *round,
                    directions: Vec::new(),
                    search_result_count: 0,
                });
                entry.search_result_count += 1;
            }
            TraceEvent::Archive {
                snapshot_ref,
                final_url,
                ..
            } => {
                archived_source_count += 1;
                snapshot_urls.insert(snapshot_ref.to_string(), final_url.clone());
            }
            TraceEvent::ArchiveSkip { .. } => skipped_source_count += 1,
            TraceEvent::SnapshotNavigationExcerpt {
                snapshot_ref,
                title,
                ..
            } => {
                snapshot_titles.insert(snapshot_ref.to_string(), title.clone());
            }
            TraceEvent::ComposedResearchAnswer { comparison, .. } => {
                synthesis_rationale =
                    Some(concise_trace_text(&comparison.synthesis_rationale, 320));
            }
            TraceEvent::RunFailed { stage, message, .. } => {
                failure = Some(TraceFailureResponse {
                    stage: format!("{stage:?}"),
                    message: concise_trace_text(message, 320),
                });
            }
            _ => {}
        }
    }
    let selected_sources = trace
        .research_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            TraceEvent::SnapshotSelection { selected } => Some(selected.as_slice()),
            _ => None,
        })
        .flat_map(|selected| selected.iter())
        .take(6)
        .map(|selection| {
            let reference = selection.snapshot_ref.to_string();
            TraceSourceResponse {
                title: snapshot_titles
                    .get(&reference)
                    .cloned()
                    .unwrap_or_else(|| "已选来源".into()),
                url: snapshot_urls.get(&reference).cloned().unwrap_or_default(),
                rationale: concise_trace_text(&selection.reason, 240),
            }
        })
        .collect();
    ResearchTraceSummaryResponse {
        run_id: trace.turn.run_id.clone(),
        clarification_rationale_audit_status: trace.clarification.rationale_audit_status(),
        research_rationale_audit_status: trace.research_rationale_audit_status,
        understanding,
        rounds: rounds.into_values().collect(),
        archived_source_count,
        skipped_source_count,
        selected_sources,
        synthesis_rationale,
        failure,
    }
}

fn project_audit_entries(trace: &LoadedTurnTrace) -> Vec<TraceAuditEntryResponse> {
    let mut entries = Vec::new();
    for event in &trace.clarification_events {
        match &event.kind {
            ClarificationEventKind::ClarificationStarted {
                original_question, ..
            } => entries.push(audit_entry("dialogue", "研究开始", original_question, None)),
            ClarificationEventKind::UserMessageReceived { message, .. } => {
                entries.push(audit_entry("dialogue", "用户补充", message, None));
            }
            ClarificationEventKind::ModelUnderstanding {
                assistant_message,
                rationale,
                decision,
                ..
            } => entries.push(audit_entry(
                "dialogue",
                match decision {
                    traceable_search::ClarificationDecision::ContinueDialogue => "模型继续对话",
                    traceable_search::ClarificationDecision::StartResearch => "模型启动研究",
                },
                assistant_message,
                Some(rationale),
            )),
            ClarificationEventKind::ResearchRunPrepared { run_id, .. } => entries.push(
                audit_entry("setup", "研究已准备", &format!("研究运行：{run_id}"), None),
            ),
            ClarificationEventKind::ResearchPreparationFailed { message, .. } => {
                entries.push(audit_entry("failure", "研究准备失败", message, None))
            }
            ClarificationEventKind::ResearchRunFailed { message, .. } => {
                entries.push(audit_entry("failure", "研究运行初始化失败", message, None))
            }
            ClarificationEventKind::Cancelled { .. } => {
                entries.push(audit_entry(
                    "dialogue",
                    "研究已取消",
                    "当前轮次已取消。",
                    None,
                ));
            }
            ClarificationEventKind::ModelRequestFailed { message, .. } => {
                entries.push(audit_entry("failure", "模型请求失败", message, None));
            }
        }
    }
    for envelope in &trace.research_events {
        match &envelope.event {
            TraceEvent::RunHeader { run_id, .. } => entries.push(research_audit_entry(
                envelope,
                "setup",
                "研究运行",
                &format!("运行标识：{run_id}"),
                None,
            )),
            TraceEvent::ModelCall {
                operation,
                round,
                output_chars,
                error_class,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "planning",
                "模型调用",
                &format!(
                    "阶段：{operation}；轮次：{round}；输出字符：{}{}",
                    output_chars.unwrap_or_default(),
                    error_class
                        .as_ref()
                        .map(|class| format!("；错误类别：{class:?}"))
                        .unwrap_or_default()
                ),
                None,
            )),
            TraceEvent::KnowledgeDraft { draft } => entries.push(research_audit_entry(
                envelope,
                "planning",
                "模型知识草稿依据",
                &draft.uncertainty,
                Some(&draft.basis_summary),
            )),
            TraceEvent::SearchQuery { round, query, gap } => entries.push(research_audit_entry(
                envelope,
                "planning",
                "检索计划",
                &format!("第 {round} 轮：{query}"),
                Some(gap),
            )),
            TraceEvent::SearchAttemptCompleted {
                round,
                engine,
                outcome,
                http_status,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "search",
                "搜索引擎尝试",
                &search_attempt_audit_detail(*round, *engine, outcome, *http_status),
                None,
            )),
            TraceEvent::SearchFallbackActivated {
                round,
                from_engine,
                to_engine,
                reason,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "search",
                "启用搜索回退",
                &format!(
                    "第 {round} 轮；{} 不可用，切换到 {}；{}",
                    search_engine_label(*from_engine),
                    search_engine_label(*to_engine),
                    search_unavailability_text(*reason)
                ),
                None,
            )),
            TraceEvent::SearchResult {
                search_engine,
                title,
                url,
                rank,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "search",
                "搜索结果",
                &format!(
                    "{} 排名 {rank}：{title}（{url}）",
                    search_engine_label(*search_engine)
                ),
                None,
            )),
            TraceEvent::Archive {
                final_url,
                char_len,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "archive",
                "网页已归档",
                &format!("{final_url}；正文 {char_len} 字符"),
                None,
            )),
            TraceEvent::ArchiveSkip { reason, .. } => {
                entries.push(research_audit_entry(
                    envelope,
                    "archive",
                    "网页未归档",
                    reason,
                    None,
                ));
            }
            TraceEvent::SnapshotNavigationExcerpt { title, excerpt, .. } => {
                entries.push(research_audit_entry(
                    envelope,
                    "archive",
                    "来源摘要",
                    &format!("{title}：{excerpt}"),
                    None,
                ))
            }
            TraceEvent::SnapshotSelection { selected } => {
                for selection in selected {
                    entries.push(research_audit_entry(
                        envelope,
                        "selection",
                        "选取来源",
                        &selection.snapshot_ref.to_string(),
                        Some(&selection.reason),
                    ));
                }
            }
            TraceEvent::ResearchClaim {
                text,
                origin,
                rationale,
                ..
            } => entries.push(research_audit_entry(
                envelope,
                "synthesis",
                match origin {
                    traceable_search::ResearchClaimOrigin::ModelKnowledge => "保留模型知识主张",
                    traceable_search::ResearchClaimOrigin::WebEvidence => "保留网页证据主张",
                },
                text,
                Some(rationale),
            )),
            TraceEvent::ComposedResearchAnswer { comparison, .. } => {
                entries.push(research_audit_entry(
                    envelope,
                    "synthesis",
                    "最终综合",
                    "模型知识与网页证据已完成对照。",
                    Some(&comparison.synthesis_rationale),
                ))
            }
            TraceEvent::RoundCompleted { round, .. } => entries.push(research_audit_entry(
                envelope,
                "planning",
                "检索轮次完成",
                &format!("第 {round} 轮已完成。"),
                None,
            )),
            TraceEvent::ExplorationStopped {
                completed_round,
                reason,
            } => entries.push(research_audit_entry(
                envelope,
                "planning",
                "探索停止",
                &format!(
                    "已完成 {completed_round} 轮；{}",
                    exploration_stop_reason_text(*reason)
                ),
                None,
            )),
            TraceEvent::RunFailed { stage, message, .. } => entries.push(research_audit_entry(
                envelope,
                "failure",
                "研究运行失败",
                &format!("{stage:?}：{message}"),
                None,
            )),
        }
    }
    entries
}

fn audit_entry(
    stage: &'static str,
    label: &'static str,
    detail: &str,
    rationale: Option<&str>,
) -> TraceAuditEntryResponse {
    TraceAuditEntryResponse {
        sequence: None,
        occurred_at: None,
        stage,
        label,
        detail: concise_trace_text(detail, 600),
        rationale: rationale.map(|value| concise_trace_text(value, 480)),
    }
}

fn research_audit_entry(
    envelope: &TraceEventEnvelope,
    stage: &'static str,
    label: &'static str,
    detail: &str,
    rationale: Option<&str>,
) -> TraceAuditEntryResponse {
    let mut entry = audit_entry(stage, label, detail, rationale);
    entry.sequence = Some(envelope.sequence);
    entry.occurred_at = Some(envelope.occurred_at);
    entry
}

const fn search_engine_label(engine: SearchEngine) -> &'static str {
    match engine {
        SearchEngine::Google => "Google",
        SearchEngine::Bing => "Bing",
    }
}

fn search_attempt_outcome_text(outcome: &SearchEngineAttemptOutcome) -> String {
    match outcome {
        SearchEngineAttemptOutcome::Completed { valid_result_count } => {
            format!("完成，{valid_result_count} 条有效结果")
        }
        SearchEngineAttemptOutcome::Unavailable { reason } => {
            format!("不可用，{}", search_unavailability_text(*reason))
        }
        SearchEngineAttemptOutcome::ContractRejected { reason } => {
            format!("响应被拒绝，{}", search_contract_failure_text(*reason))
        }
    }
}

fn search_attempt_audit_detail(
    round: u32,
    engine: SearchEngine,
    outcome: &SearchEngineAttemptOutcome,
    http_status: Option<u16>,
) -> String {
    format!(
        "第 {round} 轮；{}；{}{}",
        search_engine_label(engine),
        search_attempt_outcome_text(outcome),
        http_status
            .map(|status| format!("；HTTP {status}"))
            .unwrap_or_default()
    )
}

const fn search_unavailability_text(reason: SearchEngineUnavailability) -> &'static str {
    match reason {
        SearchEngineUnavailability::TransportFailure => "网络传输失败",
        SearchEngineUnavailability::RequestTimeout => "请求超时",
        SearchEngineUnavailability::RateLimited => "请求受到限流",
        SearchEngineUnavailability::ServerError => "搜索服务端错误",
        SearchEngineUnavailability::EngineUnresponsive => "引擎未响应",
    }
}

const fn search_contract_failure_text(reason: SearchBoundaryContractFailure) -> &'static str {
    match reason {
        SearchBoundaryContractFailure::EmptyQuery => "查询为空",
        SearchBoundaryContractFailure::UnexpectedHttpStatus => "HTTP 状态不符合契约",
        SearchBoundaryContractFailure::InvalidResponse => "响应结构不符合契约",
        SearchBoundaryContractFailure::EngineSelectionViolation => "返回结果来自错误引擎",
    }
}

const fn exploration_stop_reason_text(reason: ExplorationStopReason) -> &'static str {
    match reason {
        ExplorationStopReason::CompletedRounds => "已完成计划轮次",
        ExplorationStopReason::InputBudget => "已达到输入预算",
        ExplorationStopReason::SnapshotLimit => "已达到快照上限",
        ExplorationStopReason::NoNewUrls => "没有发现新的 URL",
    }
}

fn concise_trace_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    let mut output: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn validate_audit_stage(value: &str) -> Result<&str, PublicHttpError> {
    match value {
        "dialogue" | "setup" | "planning" | "search" | "archive" | "selection" | "synthesis"
        | "failure" => Ok(value),
        _ => Err(PublicHttpError::invalid_request()),
    }
}

fn authenticated_user(
    state: &DemoHostState,
    jar: &CookieJar,
) -> Result<UserAccountRecord, PublicHttpError> {
    let cookie = jar
        .get(LOGIN_COOKIE_NAME)
        .ok_or_else(PublicHttpError::unauthorized)?;
    state
        .catalog
        .authenticated_user(&hash_login_token(cookie.value()), now())
        .map_err(map_catalog_error)?
        .ok_or_else(PublicHttpError::unauthorized)
}

fn create_login_session(
    state: &DemoHostState,
    jar: CookieJar,
    user_id: &str,
    now: i64,
) -> Result<(CookieJar, String), PublicHttpError> {
    let login_token = generate_login_token();
    state
        .catalog
        .create_login_session(
            &hash_login_token(&login_token),
            user_id,
            now,
            now + LOGIN_SESSION_SECONDS,
        )
        .map_err(map_catalog_error)?;
    let cookie = Cookie::build((LOGIN_COOKIE_NAME, login_token.clone()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(state.secure_cookies)
        .max_age(CookieDuration::seconds(LOGIN_SESSION_SECONDS))
        .build();
    Ok((jar.add(cookie), login_token))
}

fn removal_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((LOGIN_COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .max_age(CookieDuration::ZERO)
        .build()
}

fn owned_turn_with_model(
    state: &DemoHostState,
    jar: &CookieJar,
    conversation_id: &str,
    turn_id: &str,
) -> Result<(UserAccountRecord, ResearchTurnRecord, ModelAccessConfig), PublicHttpError> {
    let user = authenticated_user(state, jar)?;
    let turn = state
        .catalog
        .owned_research_turn(&user.user_id, conversation_id, turn_id)
        .map_err(map_catalog_error)?;
    let profile = state
        .catalog
        .model_profile_for_turn(&user.user_id, &turn)
        .map_err(map_catalog_error)?;
    let model_access = model_access_from_profile(state, &profile)?;
    Ok((user, turn, model_access))
}

fn model_access_from_profile(
    state: &DemoHostState,
    profile: &ModelProfileRecord,
) -> Result<ModelAccessConfig, PublicHttpError> {
    let api_key = decrypt_profile_api_key(state, profile)?;
    ModelAccessConfig::new(&profile.api_base_url, api_key, &profile.model_id)
        .map_err(PublicHttpError::internal_failure)
}

fn decrypt_profile_api_key(
    state: &DemoHostState,
    profile: &ModelProfileRecord,
) -> Result<String, PublicHttpError> {
    state
        .credential_cipher
        .decrypt(
            &profile.user_id,
            &profile.profile_id,
            &profile.encrypted_api_key,
        )
        .map_err(PublicHttpError::internal_failure)
}

async fn validate_model_access(
    state: &DemoHostState,
    api_base_url: &str,
    api_key: &str,
    model_id: &str,
) -> Result<String, PublicHttpError> {
    if api_base_url.chars().count() > MAX_API_BASE_URL_CHARS {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_model_endpoint",
            "模型 API 地址长度无效",
        ));
    }
    let mut parsed = Url::parse(api_base_url.trim()).map_err(|_| {
        PublicHttpError::bounded_bad_request("invalid_model_endpoint", "模型 API 地址无效")
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_model_endpoint",
            "模型 API 地址必须是无凭据、查询或片段的 HTTP(S) 地址",
        ));
    }
    if !parsed.path().ends_with('/') {
        let normalized_path = format!("{}/", parsed.path());
        parsed.set_path(&normalized_path);
    }
    let normalized = parsed.to_string();
    ModelAccessConfig::new(&normalized, api_key, model_id).map_err(|_| {
        PublicHttpError::bounded_bad_request(
            "invalid_model_endpoint",
            "模型 API 地址或模型 ID 无效",
        )
    })?;
    if !state.allow_private_model_endpoints {
        validate_public_web_url(&normalized).await.map_err(|_| {
            PublicHttpError::bounded_bad_request(
                "private_model_endpoint_blocked",
                "模型 API 地址必须指向公网；本地端点需由部署管理员启用",
            )
        })?;
    }
    Ok(normalized)
}

async fn build_conversation_detail(
    state: &DemoHostState,
    conversation: ResearchConversationRecord,
) -> Result<ConversationDetailResponse, PublicHttpError> {
    let turns = state
        .catalog
        .list_research_turns(&conversation.conversation_id)
        .map_err(map_catalog_error)?;
    let mut responses = Vec::with_capacity(turns.len());
    for turn in turns {
        let clarification = state
            .research_runtime
            .load_clarification(&turn.clarification_id)
            .await
            .map_err(PublicHttpError::internal_failure)?;
        responses.push(project_research_turn(turn, Some(clarification))?);
    }
    Ok(ConversationDetailResponse {
        conversation: project_conversation_summary(conversation),
        turns: responses,
    })
}

fn project_user_account(account: UserAccountRecord) -> UserAccountResponse {
    UserAccountResponse {
        user_id: account.user_id,
        email: account.normalized_email,
        display_name: account.display_name,
        created_at: account.created_at,
    }
}

fn project_model_profile(profile: ModelProfileRecord) -> ModelProfileResponse {
    ModelProfileResponse {
        profile_id: profile.profile_id,
        display_name: profile.display_name,
        api_base_url: profile.api_base_url,
        model_id: profile.model_id,
        revision: profile.revision,
        is_default: profile.is_default,
        has_api_key: !profile.encrypted_api_key.ciphertext.is_empty(),
        verified_at: profile.verified_at,
        created_at: profile.created_at,
        updated_at: profile.updated_at,
    }
}

fn project_conversation_summary(
    conversation: ResearchConversationRecord,
) -> ConversationSummaryResponse {
    ConversationSummaryResponse {
        conversation_id: conversation.conversation_id,
        title: conversation.title,
        model_profile_id: conversation.model_profile_id,
        model_profile_name: conversation.model_profile_name,
        turn_count: conversation.turn_count,
        latest_turn_status: conversation.latest_turn_status,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    }
}

fn project_research_turn(
    turn: ResearchTurnRecord,
    clarification: Option<ClarificationState>,
) -> Result<ResearchTurnResponse, PublicHttpError> {
    let complete_answer = turn
        .answer_json
        .as_deref()
        .map(serde_json::from_str::<ResearchAnswerResponse>)
        .transpose()
        .map_err(PublicHttpError::internal_failure)?;
    let answer = complete_answer.as_ref().map(project_chat_research_answer);
    let dialogue = clarification
        .map(|clarification| project_dialogue(clarification, turn.status))
        .transpose()?;
    Ok(ResearchTurnResponse {
        turn_id: turn.turn_id,
        turn_number: turn.turn_number,
        user_question: turn.user_question,
        status: turn.status.as_str(),
        answer,
        dialogue,
        created_at: turn.created_at,
        updated_at: turn.updated_at,
        completed_at: turn.completed_at,
    })
}

fn read_jsonl_events<T: serde::de::DeserializeOwned>(
    path: impl AsRef<std::path::Path>,
) -> Result<Vec<T>, PublicHttpError> {
    let contents = fs::read_to_string(path).map_err(PublicHttpError::internal_failure)?;
    if !contents.ends_with('\n') {
        return Err(PublicHttpError::internal_failure("truncated audit trace"));
    }
    contents
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<T>, _>>()
        .map_err(PublicHttpError::internal_failure)
}

fn project_dialogue(
    clarification: ClarificationState,
    turn_status: ResearchTurnStatus,
) -> Result<TurnDialogueResponse, PublicHttpError> {
    let (status, failure) = dialogue_status_and_failure(turn_status, &clarification);
    Ok(TurnDialogueResponse {
        revision: clarification.revision,
        status,
        messages: clarification.dialogue,
        failure,
    })
}

fn dialogue_status_and_failure(
    turn_status: ResearchTurnStatus,
    clarification: &ClarificationState,
) -> (&'static str, Option<String>) {
    if let Some((status, failure)) = terminal_dialogue_outcome(turn_status) {
        return (status, Some(failure.into()));
    }
    match clarification.status {
        ClarificationStatus::ModelEvaluationPending => ("thinking", None),
        ClarificationStatus::AwaitingUserMessage => ("awaiting_message", None),
        ClarificationStatus::ResearchReady | ClarificationStatus::ResearchPrepared => {
            ("research_started", None)
        }
        ClarificationStatus::ResearchFailed => {
            ("failed", Some(AUTOMATIC_RESEARCH_FAILURE_MESSAGE.into()))
        }
        ClarificationStatus::ModelRequestFailed => (
            "failed",
            Some(
                clarification
                    .failure
                    .clone()
                    .unwrap_or_else(|| "模型暂时无法继续理解该问题。".into()),
            ),
        ),
        ClarificationStatus::Cancelled => ("cancelled", None),
    }
}

fn terminal_dialogue_outcome(
    turn_status: ResearchTurnStatus,
) -> Option<(&'static str, &'static str)> {
    (turn_status == ResearchTurnStatus::Failed)
        .then_some(("failed", AUTOMATIC_RESEARCH_FAILURE_MESSAGE))
}

fn clarification_catalog_status(clarification: &ClarificationState) -> ResearchTurnStatus {
    match clarification.status {
        ClarificationStatus::ResearchReady => ResearchTurnStatus::Ready,
        ClarificationStatus::ResearchPrepared => ResearchTurnStatus::Running,
        ClarificationStatus::ResearchFailed => ResearchTurnStatus::Failed,
        ClarificationStatus::Cancelled => ResearchTurnStatus::Cancelled,
        ClarificationStatus::ModelRequestFailed
        | ClarificationStatus::ModelEvaluationPending
        | ClarificationStatus::AwaitingUserMessage => ResearchTurnStatus::Clarifying,
    }
}

fn normalize_email(value: &str) -> Result<String, PublicHttpError> {
    let normalized = value.trim().to_lowercase();
    let valid = normalized.chars().count() <= MAX_EMAIL_CHARS
        && normalized
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    if !valid {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_email",
            "邮箱地址无效",
        ));
    }
    Ok(normalized)
}

fn validate_password(password: &str) -> Result<(), PublicHttpError> {
    let length = password.chars().count();
    if !(MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&length) {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_password",
            "密码长度必须为 12 至 200 个字符",
        ));
    }
    Ok(())
}

fn validate_trimmed_text(
    value: &str,
    minimum: usize,
    maximum: usize,
    code: &'static str,
    message: &'static str,
) -> Result<String, PublicHttpError> {
    let trimmed = value.trim();
    if !(minimum..=maximum).contains(&trimmed.chars().count()) {
        return Err(PublicHttpError::bounded_bad_request(code, message));
    }
    Ok(trimmed.to_owned())
}

fn map_catalog_error(error: CatalogError) -> PublicHttpError {
    match error {
        CatalogError::NotFound => PublicHttpError::not_found(),
        CatalogError::Conflict(message) => PublicHttpError::conflict("catalog_conflict", message),
        other => PublicHttpError::internal_failure(other),
    }
}

fn invalid_credentials() -> PublicHttpError {
    PublicHttpError {
        status: StatusCode::UNAUTHORIZED,
        code: "invalid_credentials",
        public_message: "邮箱或密码错误",
        retryable: false,
    }
}

fn model_verification_failed() -> PublicHttpError {
    PublicHttpError::bounded_bad_request(
        "model_verification_failed",
        "模型连接失败，请检查地址、密钥和模型 ID",
    )
}

fn now() -> i64 {
    Utc::now().timestamp()
}

fn new_public_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_summary_projection_has_no_search_engine_or_fallback_fields() {
        let value = serde_json::to_value(ResearchTraceSummaryResponse {
            run_id: Some("run-1".into()),
            clarification_rationale_audit_status: RationaleAuditStatus::RequiredAndValidated,
            research_rationale_audit_status: Some(RationaleAuditStatus::RequiredAndValidated),
            understanding: None,
            rounds: Vec::new(),
            archived_source_count: 0,
            skipped_source_count: 0,
            selected_sources: Vec::new(),
            synthesis_rationale: None,
            failure: None,
        })
        .unwrap();
        let serialized = serde_json::to_string(&value).unwrap();

        assert!(!serialized.contains("engine"));
        assert!(!serialized.contains("attempt"));
        assert!(!serialized.contains("fallback"));
    }

    #[test]
    fn trace_audit_entry_exposes_v7_order_time_and_search_outcome() {
        let occurred_at = Utc::now();
        let envelope = TraceEventEnvelope {
            sequence: 7,
            occurred_at,
            event: TraceEvent::SearchAttemptCompleted {
                round: 2,
                query: "primary source".into(),
                engine: SearchEngine::Google,
                outcome: SearchEngineAttemptOutcome::Unavailable {
                    reason: SearchEngineUnavailability::RateLimited,
                },
                http_status: Some(429),
            },
        };
        let detail = search_attempt_audit_detail(
            2,
            SearchEngine::Google,
            &SearchEngineAttemptOutcome::Unavailable {
                reason: SearchEngineUnavailability::RateLimited,
            },
            Some(429),
        );
        let value = serde_json::to_value(research_audit_entry(
            &envelope,
            "search",
            "搜索引擎尝试",
            &detail,
            None,
        ))
        .unwrap();

        assert_eq!(value["sequence"], 7);
        assert_eq!(
            value["occurred_at"],
            serde_json::to_value(occurred_at).unwrap()
        );
        assert_eq!(
            value["detail"],
            "第 2 轮；Google；不可用，请求受到限流；HTTP 429"
        );
    }

    #[test]
    fn email_and_password_validation_are_bounded() {
        assert_eq!(
            normalize_email(" User@Example.COM ").unwrap(),
            "user@example.com"
        );
        assert!(normalize_email("not-an-email").is_err());
        assert!(validate_password("short").is_err());
        assert!(validate_password("a long enough password").is_ok());
    }

    #[test]
    fn profile_projection_never_contains_plaintext_credentials() {
        let response = project_model_profile(ModelProfileRecord {
            profile_id: "p".repeat(32),
            user_id: "u".repeat(32),
            display_name: "Primary".into(),
            api_base_url: "https://example.com/v1/".into(),
            model_id: "model".into(),
            encrypted_api_key: crate::security::EncryptedCredential {
                ciphertext: vec![1, 2, 3],
                nonce: [4; 12],
            },
            revision: 1,
            is_default: true,
            created_at: 1,
            updated_at: 1,
            verified_at: None,
        });
        let json = serde_json::to_string(&response).unwrap();
        assert!(response.has_api_key);
        assert!(!json.contains("ciphertext"));
        assert!(!json.contains("nonce"));
        assert!(!json.contains("\"api_key\":"));
    }

    #[test]
    fn chat_turn_projection_exposes_only_l1_fields() {
        let complete_answer = serde_json::json!({
            "answer_style": "web_first",
            "answer": "Grounded answer",
            "knowledge_draft": {
                "answer": "draft",
                "claims": ["draft claim"],
                "uncertainty": "uncertain",
                "basis_summary": "review-safe basis"
            },
            "comparison": {
                "agreements": [],
                "differences": [],
                "synthesis_rationale": "review-safe synthesis"
            },
            "claims": [{
                "text": "Grounded claim",
                "origin": "web_evidence",
                "rationale": "review-safe claim rationale",
                "sources": [{"url": "https://example.com/", "title": "Example"}]
            }]
        });
        let response = project_research_turn(
            ResearchTurnRecord {
                turn_id: "t".repeat(32),
                conversation_id: "c".repeat(32),
                turn_number: 1,
                clarification_id: "i".repeat(32),
                run_id: Some("r".repeat(32)),
                user_question: "question".into(),
                status: ResearchTurnStatus::Completed,
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile_id: "p".repeat(32),
                model_profile_revision: 1,
                model_api_base_url: "https://model.example/v1/".into(),
                model_id: "secret-model-id".into(),
                answer_json: Some(complete_answer.to_string()),
                created_at: 1,
                updated_at: 2,
                completed_at: Some(2),
            },
            None,
        )
        .unwrap();
        let value = serde_json::to_value(response).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            [
                "answer",
                "completed_at",
                "created_at",
                "dialogue",
                "status",
                "turn_id",
                "turn_number",
                "updated_at",
                "user_question",
            ]
        );
        assert_eq!(
            value["answer"],
            serde_json::json!({
                "answer": "Grounded answer",
                "sources": [{"url": "https://example.com/", "title": "Example"}]
            })
        );
    }

    #[test]
    fn automatic_execution_only_resumes_nonterminal_model_ready_work() {
        assert!(is_automatic_execution_pending(
            ResearchTurnStatus::Ready,
            ClarificationStatus::ResearchReady,
        ));
        assert!(is_automatic_execution_pending(
            ResearchTurnStatus::Running,
            ClarificationStatus::ResearchPrepared,
        ));
        assert!(!is_automatic_execution_pending(
            ResearchTurnStatus::Failed,
            ClarificationStatus::ResearchPrepared,
        ));
        assert!(!is_automatic_execution_pending(
            ResearchTurnStatus::Ready,
            ClarificationStatus::AwaitingUserMessage,
        ));
    }

    #[test]
    fn automatic_failure_is_never_presented_as_research_started() {
        assert_eq!(
            terminal_dialogue_outcome(ResearchTurnStatus::Failed),
            Some(("failed", AUTOMATIC_RESEARCH_FAILURE_MESSAGE)),
        );
        assert_eq!(terminal_dialogue_outcome(ResearchTurnStatus::Running), None,);
    }
}
