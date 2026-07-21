use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::Arc,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{FromRequest, Path, RawQuery, Request, State},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use axum_extra::extract::{
    CookieJar,
    cookie::{Cookie, SameSite},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use time::Duration as CookieDuration;
use traceable_search::{
    ClarificationEvent, ClarificationEventKind, ExplorationStopReason, ModelAccessConfig,
    OpenAiCompatibleModelClient, SearchBoundaryContractFailure, SearchEngine,
    SearchEngineAttemptOutcome, SearchEngineUnavailability, TraceEvent, TraceEventEnvelope,
    replay_trace, validate_public_web_url,
};
use url::Url;
use uuid::Uuid;

use crate::{
    DemoHostState, ErrorResponse, PublicHttpError,
    catalog::{
        ArchivedConversationRecord, ArchivedModelProfileRecord, CatalogConflict, CatalogError,
        DurableIdempotencyClaim, DurableIdempotencyCompletion, ModelProfileRecord,
        NewDurableIdempotencyClaim, NewModelProfile, ResearchTurnRecord, UpdatedModelProfile,
        UserAccountRecord,
    },
    security::{generate_login_token, hash_login_token, hash_password, password_matches},
    research_service::{
        ConversationDetailResponse, ConversationSummaryResponse, CreateConversationRequest,
        CreateDialogueTurnRequest, DialogueMessageRequest, DurableIdempotencyLease,
        DurableIdempotencyStart, ResearchService, project_conversation_summary,
    },
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
const MAX_API_KEY_CHARS: usize = 4_096;
const MAX_CONVERSATION_TITLE_CHARS: usize = 200;
const IDEMPOTENCY_RETENTION_SECONDS: i64 = 24 * 60 * 60;

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
            "/archives/model-profiles",
            get(list_archived_model_profiles),
        )
        .route(
            "/model-profiles/{profile_id}/restore",
            post(restore_model_profile),
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
            get(list_conversations).post(create_conversation_durable),
        )
        .route(
            "/conversations/{conversation_id}",
            get(load_conversation)
                .patch(update_conversation)
                .delete(archive_conversation),
        )
        .route("/archives/conversations", get(list_archived_conversations))
        .route(
            "/conversations/{conversation_id}/restore",
            post(restore_conversation),
        )
        .route(
            "/conversations/{conversation_id}/turns",
            post(create_dialogue_turn_service),
        )
        .route(
            "/conversations/{conversation_id}/turns/{turn_id}/messages",
            post(submit_dialogue_message_service),
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

/// Thin HTTP adapter for the research use case. Authentication, JSON parsing,
/// and idempotency replay stay at the transport seam; ownership, revision
/// checks, compensation, durable commit, and projection live in the service.
async fn create_dialogue_turn_service(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(conversation_id): Path<String>,
    ApiJson(request): ApiJson<CreateDialogueTurnRequest>,
) -> Result<Response, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let resource_scope = format!("conversations/{conversation_id}/turns");
    let serialization_key = format!("{}:{resource_scope}:active", user.user_id);
    let idempotency = begin_durable_idempotent_request(
        &state,
        &headers,
        &user.user_id,
        &resource_scope,
        &request,
        Some(&serialization_key),
    )?;
    let mut lease = match idempotency {
        DurableIdempotencyStart::Claimed(lease) => lease,
        DurableIdempotencyStart::Replay(response) => return Ok(response),
    };
    let service = ResearchService::new(state.clone());
    let operation = match service.create_turn(&mut lease, &conversation_id, &request).await {
        Ok(operation) => operation,
        Err(error) => return finish_durable_error(&mut lease, error),
    };
    let _ = service.schedule_automatic_research_turn(
        user.user_id,
        conversation_id,
        operation.turn,
        operation.clarification,
        operation.model_access,
    );
    Ok(Json(operation.response).into_response())
}

async fn submit_dialogue_message_service(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((conversation_id, turn_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<DialogueMessageRequest>,
) -> Result<Response, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let resource_scope = format!("conversations/{conversation_id}/turns/{turn_id}/messages");
    let idempotency = begin_durable_idempotent_request(
        &state,
        &headers,
        &user.user_id,
        &resource_scope,
        &request,
        None,
    )?;
    let mut lease = match idempotency {
        DurableIdempotencyStart::Claimed(lease) => lease,
        DurableIdempotencyStart::Replay(response) => return Ok(response),
    };
    let service = ResearchService::new(state.clone());
    let operation = match service
        .submit_message(&mut lease, &conversation_id, &turn_id, &request)
        .await
    {
        Ok(operation) => operation,
        Err(error) => return finish_durable_error(&mut lease, error),
    };
    let _ = service.schedule_automatic_research_turn(
        user.user_id,
        conversation_id,
        operation.turn,
        operation.clarification,
        operation.model_access,
    );
    Ok(Json(operation.response).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterAccountRequest {
    email: String,
    password: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateModelProfileRequest {
    display_name: String,
    api_base_url: String,
    api_key: String,
    model_id: String,
    #[serde(default)]
    make_default: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct UpdateConversationRequest {
    title: Option<String>,
    model_profile_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RestoreConversationRequest {
    model_profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ResearchTraceSummaryResponse {
    model_id: String,
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

#[derive(Debug, Default)]
struct TraceAuditQuery {
    stage: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ArchivedModelProfileResponse {
    #[serde(flatten)]
    profile: ModelProfileResponse,
    archived_at: i64,
}

#[derive(Debug, Serialize)]
struct ArchivedConversationResponse {
    #[serde(flatten)]
    conversation: ConversationSummaryResponse,
    archived_at: i64,
    model_profile_available: bool,
}

#[derive(Debug, Serialize)]
struct TraceAuditPageResponse {
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
    clarification_events: Vec<ClarificationEvent>,
    research_events: Vec<TraceEventEnvelope>,
}

struct ApiJson<T>(T);
#[derive(Debug)]
struct OptionalApiJson<T>(Option<T>);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = PublicHttpError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(request, state).await.map_err(|_| {
            PublicHttpError::bounded_bad_request("invalid_json", "Request body must be valid JSON")
        })?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequest<S> for OptionalApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = PublicHttpError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = request.into_parts();
        let bytes = to_bytes(body, 16 * 1024).await.map_err(|_| {
            PublicHttpError::bounded_bad_request("invalid_json", "Request body must be valid JSON")
        })?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(None));
        }
        let request = Request::from_parts(parts, Body::from(bytes));
        let Json(value) = Json::<T>::from_request(request, state).await.map_err(|_| {
            PublicHttpError::bounded_bad_request("invalid_json", "Request body must be valid JSON")
        })?;
        Ok(Self(Some(value)))
    }
}

async fn register_account(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    ApiJson(request): ApiJson<RegisterAccountRequest>,
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
    ApiJson(request): ApiJson<LoginRequest>,
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
    authenticated_user(&state, &jar)?;
    let cookie = jar
        .get(LOGIN_COOKIE_NAME)
        .ok_or_else(PublicHttpError::unauthorized)?;
    state
        .catalog
        .revoke_login_session(&hash_login_token(cookie.value()), now())
        .map_err(map_catalog_error)?;
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

async fn list_archived_model_profiles(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<Json<Vec<ArchivedModelProfileResponse>>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let profiles = state
        .catalog
        .list_archived_model_profiles(&user.user_id)
        .map_err(map_catalog_error)?
        .into_iter()
        .map(project_archived_model_profile)
        .collect();
    Ok(Json(profiles))
}

async fn create_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateModelProfileRequest>,
) -> Result<Response, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let idempotency = begin_durable_idempotent_request(
        &state,
        &headers,
        &user.user_id,
        "model-profiles",
        &request,
        None,
    )?;
    let mut lease = match idempotency {
        DurableIdempotencyStart::Claimed(lease) => lease,
        DurableIdempotencyStart::Replay(response) => return Ok(response),
    };
    let prepared = async {
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
        validate_api_key(&request.api_key)?;
        let api_base_url =
            validate_model_access(&state, &request.api_base_url, &request.api_key, &model_id)
                .await?;
        let profile_id = lease.operation_id.clone();
        let encrypted = state
            .credential_cipher
            .encrypt(&user.user_id, &profile_id, &request.api_key)
            .map_err(PublicHttpError::internal_failure)?;
        Ok((profile_id, display_name, api_base_url, model_id, encrypted))
    }
    .await;
    let (profile_id, display_name, api_base_url, model_id, encrypted) = match prepared {
        Ok(value) => value,
        Err(error) => return finish_durable_error(&mut lease, error),
    };
    let completion = DurableIdempotencyCompletion {
        user_id: &lease.user_id,
        method: "POST",
        resource_scope: &lease.resource_scope,
        key: &lease.key,
        operation_id: &lease.operation_id,
        operation_created_at: lease.operation_created_at,
        claim_token: &lease.claim_token,
        status_code: i64::from(StatusCode::OK.as_u16()),
    };
    let commit = state.catalog.commit_model_profile_idempotent(
        completion,
        NewModelProfile {
            profile_id: &profile_id,
            user_id: &user.user_id,
            display_name: &display_name,
            api_base_url: &api_base_url,
            model_id: &model_id,
            encrypted_api_key: &encrypted,
            make_default: request.make_default,
            now: lease.operation_created_at,
        },
        |profile| project_model_profile(profile.clone()),
    );
    let commit = match commit {
        Ok(commit) => commit,
        Err(error) => return finish_durable_error(&mut lease, map_catalog_error(error)),
    };
    lease.completed = true;
    Ok(Json(commit.projection).into_response())
}

async fn update_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
    ApiJson(request): ApiJson<UpdateModelProfileRequest>,
) -> Result<Json<ModelProfileResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    if request.display_name.is_none()
        && request.api_base_url.is_none()
        && request.api_key.is_none()
        && request.model_id.is_none()
    {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_request",
            "At least one editable model profile field is required",
        ));
    }
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
        Some(value) => {
            validate_api_key(&value)?;
            value
        }
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
            expected_revision: existing.revision,
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
    let client = if state.allow_private_model_endpoints {
        OpenAiCompatibleModelClient::new(&profile.api_base_url, api_key, &profile.model_id)
    } else {
        OpenAiCompatibleModelClient::new_public(&profile.api_base_url, api_key, &profile.model_id)
    }
    .map_err(|_| model_verification_failed())?;
    client
        .generate_structured_output::<serde_json::Value>("Return JSON only.", r#"{"ok":true}"#)
        .await
        .map_err(|_| model_verification_failed())?;
    state
        .catalog
        .mark_model_profile_verified(&user.user_id, &profile_id, profile.revision, now())
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

async fn restore_model_profile(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(profile_id): Path<String>,
) -> Result<Json<ModelProfileResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let profile = state
        .catalog
        .restore_model_profile(&user.user_id, &profile_id, now())
        .map_err(map_catalog_error)?;
    Ok(Json(project_model_profile(profile)))
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

async fn list_archived_conversations(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
) -> Result<Json<Vec<ArchivedConversationResponse>>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let conversations = state
        .catalog
        .list_archived_research_conversations(&user.user_id)
        .map_err(map_catalog_error)?
        .into_iter()
        .map(project_archived_conversation)
        .collect();
    Ok(Json(conversations))
}

async fn create_conversation_durable(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateConversationRequest>,
) -> Result<Response, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let idempotency = begin_durable_idempotent_request(
        &state,
        &headers,
        &user.user_id,
        "conversations",
        &request,
        None,
    )?;
    let mut lease = match idempotency {
        DurableIdempotencyStart::Claimed(lease) => lease,
        DurableIdempotencyStart::Replay(response) => return Ok(response),
    };
    let conversation = match ResearchService::new(state.clone())
        .create_conversation(&mut lease, &request)
        .await
    {
        Ok(conversation) => conversation,
        Err(error) => return finish_durable_error(&mut lease, error),
    };
    Ok(Json(conversation).into_response())
}

async fn load_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
) -> Result<Json<ConversationDetailResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    Ok(Json(
        ResearchService::new(state)
            .load_conversation(&user.user_id, &conversation_id)
            .await?,
    ))
}

async fn update_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
    ApiJson(request): ApiJson<UpdateConversationRequest>,
) -> Result<Json<ConversationSummaryResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    if request.title.is_none() && request.model_profile_id.is_none() {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_request",
            "At least one editable conversation field is required",
        ));
    }
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

async fn restore_conversation(
    State(state): State<Arc<DemoHostState>>,
    jar: CookieJar,
    Path(conversation_id): Path<String>,
    OptionalApiJson(request): OptionalApiJson<RestoreConversationRequest>,
) -> Result<Json<ConversationSummaryResponse>, PublicHttpError> {
    let user = authenticated_user(&state, &jar)?;
    let model_profile_id = request.and_then(|body| body.model_profile_id);
    let restored = state
        .catalog
        .restore_research_conversation(
            &user.user_id,
            &conversation_id,
            model_profile_id.as_deref(),
            now(),
        )
        .map_err(map_catalog_error)?;
    Ok(Json(project_conversation_summary(restored)))
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
    RawQuery(raw_query): RawQuery,
) -> Result<Json<TraceAuditPageResponse>, PublicHttpError> {
    let query = parse_trace_audit_query(raw_query.as_deref())?;
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
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_trace_cursor",
            "Trace audit cursor is outside the available range",
        ));
    }
    let limit = query.limit.unwrap_or(40);
    if !(1..=100).contains(&limit) {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_trace_limit",
            "Trace audit limit must be between 1 and 100",
        ));
    }
    let end = cursor.saturating_add(limit).min(filtered.len());
    Ok(Json(TraceAuditPageResponse {
        next_cursor: (end < filtered.len()).then_some(end),
        entries: filtered[cursor..end].to_vec(),
    }))
}

fn parse_trace_audit_query(raw_query: Option<&str>) -> Result<TraceAuditQuery, PublicHttpError> {
    let mut query = TraceAuditQuery::default();
    for (key, value) in url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "stage" => {
                if query.stage.replace(value.into_owned()).is_some() {
                    return Err(PublicHttpError::bounded_bad_request(
                        "invalid_trace_stage",
                        "Trace audit stage is invalid",
                    ));
                }
            }
            "cursor" => {
                if query.cursor.is_some() {
                    return Err(PublicHttpError::bounded_bad_request(
                        "invalid_trace_cursor",
                        "Trace audit cursor is invalid",
                    ));
                }
                query.cursor = Some(value.parse::<usize>().map_err(|_| {
                    PublicHttpError::bounded_bad_request(
                        "invalid_trace_cursor",
                        "Trace audit cursor is invalid",
                    )
                })?);
            }
            "limit" => {
                if query.limit.is_some() {
                    return Err(PublicHttpError::bounded_bad_request(
                        "invalid_trace_limit",
                        "Trace audit limit is invalid",
                    ));
                }
                query.limit = Some(value.parse::<usize>().map_err(|_| {
                    PublicHttpError::bounded_bad_request(
                        "invalid_trace_limit",
                        "Trace audit limit is invalid",
                    )
                })?);
            }
            _ => {}
        }
    }
    Ok(query)
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
    let clarification_events = read_jsonl_events::<ClarificationEvent>(
        state
            .research_runtime
            .clarification_trace_path(&turn.clarification_id),
    )?;
    let research_events = if let Some(run_id) = &turn.run_id {
        let replayed = replay_trace(state.research_runtime.research_trace_path(run_id))
            .map_err(|error| PublicHttpError::internal_failure(error.to_string()))?;
        if replayed.header.run_id != *run_id
            || replayed.header.clarification_id != turn.clarification_id
        {
            return Err(PublicHttpError::internal_failure(
                "research trace header does not match the owned turn",
            ));
        }
        replayed.events
    } else {
        Vec::new()
    };
    Ok(LoadedTurnTrace {
        turn,
        clarification_events,
        research_events,
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
        model_id: trace.turn.model_id.clone(),
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
            ClarificationEventKind::ResearchRunPrepared { .. } => entries.push(audit_entry(
                "setup",
                "研究已准备",
                "研究计划已准备完成。",
                None,
            )),
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
            TraceEvent::RunHeader { .. } => entries.push(research_audit_entry(
                envelope,
                "setup",
                "研究运行",
                "研究运行已开始。",
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
        SearchEngine::Brave => "Brave",
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
        _ => Err(PublicHttpError::bounded_bad_request(
            "invalid_trace_stage",
            "Trace audit stage is invalid",
        )),
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

fn project_archived_model_profile(
    profile: ArchivedModelProfileRecord,
) -> ArchivedModelProfileResponse {
    ArchivedModelProfileResponse {
        archived_at: profile.archived_at,
        profile: project_model_profile(profile.profile),
    }
}

fn project_archived_conversation(
    conversation: ArchivedConversationRecord,
) -> ArchivedConversationResponse {
    ArchivedConversationResponse {
        archived_at: conversation.archived_at,
        model_profile_available: conversation.model_profile_available,
        conversation: project_conversation_summary(conversation.conversation),
    }
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

fn begin_durable_idempotent_request<T: Serialize>(
    state: &Arc<DemoHostState>,
    headers: &HeaderMap,
    user_id: &str,
    resource_scope: &str,
    request: &T,
    serialization_key: Option<&str>,
) -> Result<DurableIdempotencyStart, PublicHttpError> {
    let key = headers
        .get("idempotency-key")
        .ok_or_else(|| {
            PublicHttpError::bounded_bad_request(
                "idempotency_key_required",
                "Idempotency-Key is required",
            )
        })?
        .to_str()
        .map_err(|_| {
            PublicHttpError::bounded_bad_request(
                "invalid_idempotency_key",
                "Idempotency-Key is invalid",
            )
        })?
        .trim();
    if !(8..=128).contains(&key.len())
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key is invalid",
        ));
    }
    let request_bytes = serde_json::to_vec(request).map_err(PublicHttpError::internal_failure)?;
    let request_hash = format!("{:x}", Sha256::digest(request_bytes));
    let current_time = now();
    match state
        .catalog
        .claim_operation(NewDurableIdempotencyClaim {
            user_id,
            method: "POST",
            resource_scope,
            key,
            request_hash: &request_hash,
            serialization_key,
            now: current_time,
            expires_at: current_time + IDEMPOTENCY_RETENTION_SECONDS,
        })
        .map_err(map_catalog_error)?
    {
        DurableIdempotencyClaim::Claimed(lease) => Ok(DurableIdempotencyStart::Claimed(
            DurableIdempotencyLease {
                state: Arc::clone(state),
                user_id: user_id.to_owned(),
                resource_scope: resource_scope.to_owned(),
                key: key.to_owned(),
                operation_id: lease.operation_id,
                operation_created_at: lease.operation_created_at,
                claim_token: lease.claim_token,
                completed: false,
            },
        )),
        DurableIdempotencyClaim::InProgress { .. } => Err(PublicHttpError {
            status: StatusCode::CONFLICT,
            code: "idempotency_request_in_progress",
            public_message: "The original request is still in progress",
            retryable: true,
        }),
        DurableIdempotencyClaim::Blocked { .. } => Err(PublicHttpError {
            status: StatusCode::CONFLICT,
            code: "idempotency_operation_blocked",
            public_message: "The original request requires operator recovery",
            retryable: false,
        }),
        DurableIdempotencyClaim::Reused => Err(PublicHttpError::conflict(
            "idempotency_key_reused",
            "Idempotency-Key was already used for a different request",
        )),
        DurableIdempotencyClaim::Replay {
            status_code,
            response_json,
            ..
        } => {
            let status = StatusCode::from_u16(
                u16::try_from(status_code).map_err(PublicHttpError::internal_failure)?,
            )
            .map_err(PublicHttpError::internal_failure)?;
            let response = Response::builder()
                .status(status)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(response_json))
                .map_err(PublicHttpError::internal_failure)?;
            Ok(DurableIdempotencyStart::Replay(response))
        }
    }
}

fn finish_durable_error(
    lease: &mut DurableIdempotencyLease,
    error: PublicHttpError,
) -> Result<Response, PublicHttpError> {
    if lease.completed {
        return Err(error);
    }
    let response_json = serde_json::to_string(&ErrorResponse {
        code: error.code,
        message: error.public_message,
        retryable: error.retryable,
    })
    .map_err(PublicHttpError::internal_failure)?;
    lease
        .state
        .catalog
        .complete_durable_idempotency(
            DurableIdempotencyCompletion {
                user_id: &lease.user_id,
                method: "POST",
                resource_scope: &lease.resource_scope,
                key: &lease.key,
                operation_id: &lease.operation_id,
                operation_created_at: lease.operation_created_at,
                claim_token: &lease.claim_token,
                status_code: i64::from(error.status.as_u16()),
            },
            &response_json,
        )
        .map_err(map_catalog_error)?;
    lease.completed = true;
    Err(error)
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

fn validate_api_key(api_key: &str) -> Result<(), PublicHttpError> {
    if api_key.trim().is_empty() || api_key.chars().count() > MAX_API_KEY_CHARS {
        return Err(PublicHttpError::bounded_bad_request(
            "invalid_api_key",
            "Model API key must be between 1 and 4096 characters",
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
        CatalogError::Conflict(CatalogConflict::EmailAlreadyRegistered) => {
            PublicHttpError::conflict(
                "email_already_registered",
                "An account already exists for this email",
            )
        }
        CatalogError::Conflict(CatalogConflict::ModelProfileNameAlreadyExists) => {
            PublicHttpError::conflict(
                "profile_name_already_exists",
                "A model profile with this name already exists",
            )
        }
        CatalogError::Conflict(CatalogConflict::ModelProfileInUseByActiveTurn) => {
            PublicHttpError::conflict(
                "model_profile_in_use_by_active_turn",
                "Finish or cancel active turns before changing this model profile",
            )
        }
        CatalogError::Conflict(CatalogConflict::ModelProfileInUseByConversation) => {
            PublicHttpError::conflict(
                "model_profile_in_use_by_conversation",
                "Choose another model for active conversations before archiving this profile",
            )
        }
        CatalogError::Conflict(CatalogConflict::ConversationHasActiveTurn) => {
            PublicHttpError::conflict(
                "conversation_has_active_turn",
                "Finish or cancel the active turn before changing this conversation",
            )
        }
        CatalogError::Conflict(CatalogConflict::ConversationModelProfileArchived) => {
            PublicHttpError::conflict(
                "conversation_model_profile_archived",
                "Choose an active model profile before restoring this conversation",
            )
        }
        CatalogError::Conflict(CatalogConflict::ConversationModelProfileChanged) => {
            PublicHttpError::conflict(
                "conversation_model_profile_changed",
                "The conversation model profile changed before this research turn was created",
            )
        }
        CatalogError::Conflict(CatalogConflict::ModelProfileChanged) => PublicHttpError::conflict(
            "model_profile_changed",
            "The model profile changed; retry using the latest values",
        ),
        CatalogError::Conflict(CatalogConflict::ResearchTurnStatusChanged) => {
            PublicHttpError::conflict(
                "turn_not_accepting_messages",
                "This research turn is no longer accepting messages",
            )
        }
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

    #[tokio::test]
    async fn optional_json_accepts_empty_body_and_requires_json_content_type_when_present() {
        let request = Request::builder().body(Body::empty()).unwrap();
        let OptionalApiJson(body) =
            OptionalApiJson::<RestoreConversationRequest>::from_request(request, &())
                .await
                .unwrap();
        assert!(body.is_none());

        let request = Request::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        let error = OptionalApiJson::<RestoreConversationRequest>::from_request(request, &())
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_json");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let request = Request::builder()
            .header(CONTENT_TYPE, "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let error = OptionalApiJson::<RestoreConversationRequest>::from_request(request, &())
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_json");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn trace_summary_projection_has_no_search_engine_or_fallback_fields() {
        let value = serde_json::to_value(ResearchTraceSummaryResponse {
            model_id: "model-1".into(),
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
        assert!(!serialized.contains("run_id"));
        assert!(!serialized.contains("audit_status"));
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
    fn atomic_turn_guards_map_to_the_frozen_conflict_codes() {
        for (conflict, expected_code) in [
            (
                CatalogConflict::ConversationModelProfileChanged,
                "conversation_model_profile_changed",
            ),
            (
                CatalogConflict::ModelProfileChanged,
                "model_profile_changed",
            ),
            (
                CatalogConflict::ConversationHasActiveTurn,
                "conversation_has_active_turn",
            ),
            (
                CatalogConflict::ResearchTurnStatusChanged,
                "turn_not_accepting_messages",
            ),
        ] {
            let error = map_catalog_error(CatalogError::Conflict(conflict));
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(error.code, expected_code);
        }
    }
}
