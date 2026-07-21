use std::sync::Arc;

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use traceable_search::{
    ChatResearchAnswerResponse, ClarificationError, ClarificationState, ClarificationStatus,
    ConversationError, DialogueMessage, ModelAccessConfig, ResearchAnswerResponse,
    ResearchAnswerStyle, ResearchRuntimeError, TracePolicy, project_chat_research_answer,
};

use crate::{
    DemoHostState, PublicHttpError,
    catalog::{
        CatalogConflict, CatalogError, DurableIdempotencyClaim, DurableIdempotencyCompletion,
        DEFAULT_RESEARCH_CONVERSATION_TITLE, NewDurableIdempotencyClaim, NewResearchConversation,
        NewResearchTurn,
        ResearchConversationRecord, ResearchTurnRecord, ResearchTurnStatus,
    },
};

const AUTOMATIC_RESEARCH_FAILURE_MESSAGE: &str = "研究未能自动完成。请直接发送新的研究问题。";
const AUTOMATIC_RESEARCH_FAILURE_SUMMARY: &str = "Automatic research could not complete.";
const IDEMPOTENCY_RETENTION_SECONDS: i64 = 24 * 60 * 60;

pub(crate) enum ResearchServiceResult<T> {
    Completed(T),
    Replay(ServiceReplay),
}

pub(crate) struct ServiceReplay {
    status: StatusCode,
    response_json: String,
}

impl ServiceReplay {
    pub(crate) fn into_response(self) -> Response {
        (self.status, [(CONTENT_TYPE, "application/json")], Body::from(self.response_json))
            .into_response()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateConversationRequest {
    pub(crate) model_profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConversationSummaryResponse {
    pub(crate) conversation_id: String,
    pub(crate) title: String,
    pub(crate) model_profile_id: String,
    pub(crate) model_profile_name: String,
    pub(crate) turn_count: i64,
    pub(crate) latest_turn_status: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConversationDetailResponse {
    #[serde(flatten)]
    pub(crate) conversation: ConversationSummaryResponse,
    pub(crate) turns: Vec<ResearchTurnResponse>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDialogueTurnRequest {
    pub(crate) question: String,
    pub(crate) answer_style: ResearchAnswerStyle,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DialogueMessageRequest {
    pub(crate) revision: u32,
    pub(crate) message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TurnDialogueResponse {
    pub(crate) revision: u32,
    pub(crate) status: String,
    pub(crate) messages: Vec<DialogueMessage>,
    pub(crate) failure: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResearchTurnResponse {
    pub(crate) turn_id: String,
    pub(crate) turn_number: i64,
    pub(crate) user_question: String,
    pub(crate) status: String,
    pub(crate) answer: Option<ChatResearchAnswerResponse>,
    pub(crate) dialogue: Option<TurnDialogueResponse>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) completed_at: Option<i64>,
}

/// The result of one write-side research use case. The HTTP layer only needs
/// to project the response and, when a model has approved research, schedule
/// the already-persisted turn.
pub(crate) struct ResearchTurnOperation {
    pub(crate) turn: ResearchTurnRecord,
    pub(crate) clarification: ClarificationState,
    pub(crate) model_access: ModelAccessConfig,
    pub(crate) response: ResearchTurnResponse,
}

/// Opaque durable operation metadata passed from the idempotency claim to the
/// use-case module. Callers do not manipulate fencing or resource identities.
pub(crate) struct DurableIdempotencyLease {
    pub(crate) state: Arc<DemoHostState>,
    pub(crate) user_id: String,
    pub(crate) resource_scope: String,
    pub(crate) key: String,
    pub(crate) operation_id: String,
    pub(crate) operation_created_at: i64,
    pub(crate) claim_token: String,
    pub(crate) completed: bool,
}

impl Drop for DurableIdempotencyLease {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Err(error) = self.state.catalog.abandon_idempotency(
            &self.user_id,
            "POST",
            &self.resource_scope,
            &self.key,
            &self.claim_token,
        ) {
            tracing::error!(error = %error, "failed to abandon idempotency claim");
        }
    }
}

pub(crate) enum DurableIdempotencyStart {
    Claimed(DurableIdempotencyLease),
    Replay(axum::response::Response),
}

enum ServiceClaimStart {
    Claimed(DurableIdempotencyLease),
    Replay(ServiceReplay),
}

#[derive(Clone)]
pub(crate) struct ResearchService {
    state: Arc<DemoHostState>,
}

impl ResearchService {
    pub(crate) fn new(state: Arc<DemoHostState>) -> Self {
        Self { state }
    }

    pub(crate) async fn create_conversation(
        &self,
        user_id: &str,
        idempotency_key: Option<String>,
        request: &CreateConversationRequest,
    ) -> Result<ResearchServiceResult<ConversationDetailResponse>, PublicHttpError> {
        let mut lease = match self.begin_durable(
            user_id,
            "conversations",
            idempotency_key.as_deref(),
            request,
            None,
        )? {
            ServiceClaimStart::Claimed(lease) => lease,
            ServiceClaimStart::Replay(replay) => return Ok(ResearchServiceResult::Replay(replay)),
        };
        match self.create_conversation_claimed(&mut lease, request).await {
            Ok(response) => Ok(ResearchServiceResult::Completed(response)),
            Err(error) => Err(self.finish_error(&mut lease, error)?),
        }
    }

    async fn create_conversation_claimed(
        &self,
        lease: &mut DurableIdempotencyLease,
        request: &CreateConversationRequest,
    ) -> Result<ConversationDetailResponse, PublicHttpError> {
        let profile = match &request.model_profile_id {
            Some(profile_id) => self
                .state
                .catalog
                .model_profile(&lease.user_id, profile_id)
                .map_err(map_catalog_error)?,
            None => self
                .state
                .catalog
                .default_model_profile(&lease.user_id)
                .map_err(|error| match error {
                    CatalogError::NotFound => PublicHttpError::conflict(
                        "model_profile_required",
                        "请先添加模型配置",
                    ),
                    other => map_catalog_error(other),
                })?,
        };
        let conversation_id = lease.operation_id.clone();
        let core_conversation_id = format!(
            "session-{}",
            derive_operation_resource_id(&lease.operation_id, "core-conversation")
        );
        self.state
            .research_runtime
            .create_conversation_idempotent(
                &core_conversation_id,
                operation_datetime(lease.operation_created_at),
            )
            .await
            .map_err(PublicHttpError::internal_failure)?;
        let commit = self
            .state
            .catalog
            .commit_research_conversation_idempotent(
                durable_completion(lease),
                NewResearchConversation {
                    conversation_id: &conversation_id,
                    user_id: &lease.user_id,
                    core_conversation_id: &core_conversation_id,
                    title: DEFAULT_RESEARCH_CONVERSATION_TITLE,
                    model_profile_id: &profile.profile_id,
                    now: lease.operation_created_at,
                },
                |conversation| ConversationDetailResponse {
                    conversation: project_conversation_summary(conversation.clone()),
                    turns: Vec::new(),
                },
            )
            .map_err(map_catalog_error)?;
        lease.completed = true;
        Ok(commit.projection)
    }

    pub(crate) async fn load_conversation(
        &self,
        user_id: &str,
        conversation_id: &str,
    ) -> Result<ConversationDetailResponse, PublicHttpError> {
        let conversation = self
            .state
            .catalog
            .research_conversation(user_id, conversation_id)
            .map_err(map_catalog_error)?;
        let turns = self
            .state
            .catalog
            .list_research_turns(conversation_id)
            .map_err(map_catalog_error)?;
        let mut responses = Vec::with_capacity(turns.len());
        for turn in turns {
            let clarification = self
                .state
                .research_runtime
                .load_clarification(&turn.clarification_id)
                .await
                .map_err(PublicHttpError::internal_failure)?;
            responses.push(
                project_research_turn(turn, Some(clarification))
                    .map_err(map_catalog_error)?,
            );
        }
        Ok(ConversationDetailResponse {
            conversation: project_conversation_summary(conversation),
            turns: responses,
        })
    }

    pub(crate) async fn create_turn(
        &self,
        user_id: &str,
        conversation_id: &str,
        idempotency_key: Option<String>,
        request: &CreateDialogueTurnRequest,
    ) -> Result<ResearchServiceResult<ResearchTurnResponse>, PublicHttpError> {
        let resource_scope = format!("conversations/{conversation_id}/turns");
        let serialization_key = format!("{user_id}:{resource_scope}:active");
        let mut lease = match self.begin_durable(
            user_id,
            &resource_scope,
            idempotency_key.as_deref(),
            request,
            Some(&serialization_key),
        )? {
            ServiceClaimStart::Claimed(lease) => lease,
            ServiceClaimStart::Replay(replay) => return Ok(ResearchServiceResult::Replay(replay)),
        };
        let operation = match self
            .create_turn_claimed(&mut lease, conversation_id, request)
            .await
        {
            Ok(operation) => operation,
            Err(error) => return Err(self.finish_error(&mut lease, error)?),
        };
        let _ = self.schedule_automatic_research_turn(
            user_id.to_owned(),
            conversation_id.to_owned(),
            operation.turn,
            operation.clarification,
            operation.model_access,
        );
        Ok(ResearchServiceResult::Completed(operation.response))
    }

    async fn create_turn_claimed(
        &self,
        lease: &mut DurableIdempotencyLease,
        conversation_id: &str,
        request: &CreateDialogueTurnRequest,
    ) -> Result<ResearchTurnOperation, PublicHttpError> {
        let state = &self.state;
        let question = validate_trimmed_text(
            &request.question,
            1,
            4_000,
            "invalid_question",
            "研究问题长度无效",
        )?;
        let conversation = state
            .catalog
            .research_conversation(&lease.user_id, conversation_id)
            .map_err(map_catalog_error)?;
        let profile = state
            .catalog
            .model_profile(&lease.user_id, &conversation.model_profile_id)
            .map_err(map_catalog_error)?;
        let model_access = model_access_from_profile(state, &profile)?;
        let clarification_id = derive_operation_resource_id(&lease.operation_id, "clarification");
        let clarification = state
            .research_runtime
            .start_research_turn_idempotent(
                &conversation.core_conversation_id,
                &clarification_id,
                &lease.operation_id,
                &question,
                operation_datetime(lease.operation_created_at),
                &model_access,
            )
            .await
            .map_err(map_create_turn_runtime_error)?;

        let current_conversation = match state
            .catalog
            .research_conversation(&lease.user_id, conversation_id)
        {
            Ok(value) => value,
            Err(error) => {
                compensate_clarification(state, &clarification).await;
                return Err(map_catalog_error(error));
            }
        };
        if current_conversation.model_profile_id != conversation.model_profile_id {
            compensate_clarification(state, &clarification).await;
            return Err(PublicHttpError::conflict(
                "conversation_model_profile_changed",
                "The conversation model profile changed before this research turn was created",
            ));
        }
        let current_profile = match state
            .catalog
            .model_profile(&lease.user_id, &conversation.model_profile_id)
        {
            Ok(value) => value,
            Err(error) => {
                compensate_clarification(state, &clarification).await;
                return Err(map_catalog_error(error));
            }
        };
        if current_profile.revision != profile.revision {
            compensate_clarification(state, &clarification).await;
            return Err(PublicHttpError::conflict(
                "model_profile_changed",
                "The model profile changed; retry using the latest values",
            ));
        }
        let turn_number = clarification
            .turn
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| PublicHttpError::internal_failure("research clarification has no turn number"))?;
        let turn_id = derive_operation_resource_id(&lease.operation_id, "turn");
        let response_clarification = clarification.clone();
        let status = clarification_catalog_status(&clarification);
        let commit = state.catalog.commit_research_turn_idempotent_result(
            durable_completion(lease),
            NewResearchTurn {
                turn_id: &turn_id,
                conversation_id,
                turn_number,
                clarification_id: &clarification.clarification_id,
                user_question: &question,
                status,
                // Scope convergence freezes the first release to the web-first
                // evidence path. The request field remains accepted for client
                // compatibility but cannot select a deferred strategy.
                answer_style: ResearchAnswerStyle::WebFirst,
                model_profile: &profile,
                now: lease.operation_created_at,
            },
            move |turn| {
                serde_json::to_value(project_research_turn(
                    turn.clone(),
                    Some(response_clarification.clone()),
                )?)
                .map_err(CatalogError::ResponseSerialization)
            },
        );
        let commit = match commit {
            Ok(commit) => commit,
            Err(error) => {
                compensate_clarification(state, &clarification).await;
                return Err(map_catalog_error(error));
            }
        };
        lease.completed = true;
        Ok(ResearchTurnOperation {
            turn: commit.resource,
            clarification,
            model_access,
            response: serde_json::from_value(commit.projection)
                .map_err(PublicHttpError::internal_failure)?,
        })
    }

    pub(crate) async fn submit_message(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn_id: &str,
        idempotency_key: Option<String>,
        request: &DialogueMessageRequest,
    ) -> Result<ResearchServiceResult<ResearchTurnResponse>, PublicHttpError> {
        let resource_scope = format!("conversations/{conversation_id}/turns/{turn_id}/messages");
        let mut lease = match self.begin_durable(
            user_id,
            &resource_scope,
            idempotency_key.as_deref(),
            request,
            None,
        )? {
            ServiceClaimStart::Claimed(lease) => lease,
            ServiceClaimStart::Replay(replay) => return Ok(ResearchServiceResult::Replay(replay)),
        };
        let operation = match self
            .submit_message_claimed(&mut lease, conversation_id, turn_id, request)
            .await
        {
            Ok(operation) => operation,
            Err(error) => return Err(self.finish_error(&mut lease, error)?),
        };
        let _ = self.schedule_automatic_research_turn(
            user_id.to_owned(),
            conversation_id.to_owned(),
            operation.turn,
            operation.clarification,
            operation.model_access,
        );
        Ok(ResearchServiceResult::Completed(operation.response))
    }

    async fn submit_message_claimed(
        &self,
        lease: &mut DurableIdempotencyLease,
        conversation_id: &str,
        turn_id: &str,
        request: &DialogueMessageRequest,
    ) -> Result<ResearchTurnOperation, PublicHttpError> {
        let state = &self.state;
        let message = validate_trimmed_text(
            &request.message,
            1,
            4_000,
            "invalid_dialogue_message",
            "消息长度无效",
        )?;
        let turn = state
            .catalog
            .owned_research_turn(&lease.user_id, conversation_id, turn_id)
            .map_err(map_catalog_error)?;
        let profile = state
            .catalog
            .model_profile_for_turn(&lease.user_id, &turn)
            .map_err(map_catalog_error)?;
        let model_access = model_access_from_profile(state, &profile)?;
        let clarification = state
            .research_runtime
            .submit_dialogue_message_idempotent(
                &turn.clarification_id,
                &lease.operation_id,
                request.revision,
                &message,
                operation_datetime(lease.operation_created_at),
                &model_access,
            )
            .await
            .map_err(map_dialogue_runtime_error)?;
        let response_clarification = clarification.clone();
        let commit = state.catalog.commit_research_turn_status_idempotent_result(
            durable_completion(lease),
            &turn.turn_id,
            clarification_catalog_status(&clarification),
            None,
            None,
            lease.operation_created_at,
            move |updated| {
                serde_json::to_value(project_research_turn(
                    updated.clone(),
                    Some(response_clarification.clone()),
                )?)
                .map_err(CatalogError::ResponseSerialization)
            },
        );
        let commit = commit.map_err(map_catalog_error)?;
        lease.completed = true;
        Ok(ResearchTurnOperation {
            turn: commit.resource,
            clarification,
            model_access,
            response: serde_json::from_value(commit.projection)
                .map_err(PublicHttpError::internal_failure)?,
        })
    }

    /// Schedules research only after the Clarification model has committed a
    /// `start_research` decision. The HTTP adapter receives the already
    /// persisted turn immediately; this method owns detached execution.
    pub(crate) fn schedule_automatic_research_turn(
        &self,
        user_id: String,
        conversation_id: String,
        turn: ResearchTurnRecord,
        clarification: ClarificationState,
        model_access: ModelAccessConfig,
    ) -> (ResearchTurnRecord, ClarificationState) {
        if !is_automatic_execution_pending(turn.status, clarification.status) {
            return (turn, clarification);
        }
        let service = self.clone();
        let execution_turn = turn.clone();
        tokio::spawn(async move {
            let turn_id = execution_turn.turn_id.clone();
            if service
                .execute_scheduled_research_turn(
                    &user_id,
                    &conversation_id,
                    execution_turn,
                    &model_access,
                )
                .await
                .is_err()
            {
                tracing::error!(turn_id = %turn_id, "scheduled automatic research failed");
            }
        });
        (turn, clarification)
    }

    async fn execute_scheduled_research_turn(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn: ResearchTurnRecord,
        model_access: &ModelAccessConfig,
    ) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
        let state = &self.state;
        let permit = state
            .research_slots
            .acquire()
            .await
            .map_err(|_| PublicHttpError::internal_failure("research capacity semaphore was closed"))?;
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
            Err(_) => return self.fail_automatic_research_turn(user_id, conversation_id, turn).await,
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
            Err(_) => return self.fail_automatic_research_turn(user_id, conversation_id, turn).await,
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

    async fn fail_automatic_research_turn(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn: ResearchTurnRecord,
    ) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
        let state = &self.state;
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

    /// Reconciles persisted ready/running turns after host startup. A restart
    /// never asks the browser to confirm execution; it uses the pinned model
    /// revision and the same scheduler as a fresh Turn.
    pub(crate) fn start_automatic_execution_recovery(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let candidates = match service.state.catalog.automatic_execution_recovery_candidates() {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::error!(error = %error, "could not list automatic research recovery candidates");
                    return;
                }
            };
            for candidate in candidates {
                let turn = candidate.turn;
                let turn_id = turn.turn_id.clone();
                let conversation_id = turn.conversation_id.clone();
                let clarification = match service
                    .state
                    .research_runtime
                    .load_clarification(&turn.clarification_id)
                    .await
                {
                    Ok(clarification) => clarification,
                    Err(_) => {
                        tracing::error!(turn_id = %turn_id, "could not load automatic research recovery state");
                        continue;
                    }
                };
                if service
                    .resume_automatic_execution_if_needed(
                        &candidate.user_id,
                        &conversation_id,
                        turn,
                        clarification,
                    )
                    .await
                    .is_err()
                {
                    tracing::error!(turn_id = %turn_id, "automatic research recovery failed");
                }
            }
        });
    }

    async fn resume_automatic_execution_if_needed(
        &self,
        user_id: &str,
        conversation_id: &str,
        turn: ResearchTurnRecord,
        clarification: ClarificationState,
    ) -> Result<(ResearchTurnRecord, ClarificationState), PublicHttpError> {
        if matches!(
            clarification.status,
            ClarificationStatus::ResearchFailed | ClarificationStatus::Cancelled
        ) {
            return self
                .reconcile_terminal_automatic_turn(user_id, conversation_id, turn, clarification)
                .await;
        }
        if !is_automatic_execution_pending(turn.status, clarification.status) {
            return Ok((turn, clarification));
        }
        let profile = match self.state.catalog.model_profile_for_turn(user_id, &turn) {
            Ok(profile) => profile,
            Err(_) => return self.fail_automatic_research_turn(user_id, conversation_id, turn).await,
        };
        let model_access = match model_access_from_profile(&self.state, &profile) {
            Ok(model_access) => model_access,
            Err(_) => return self.fail_automatic_research_turn(user_id, conversation_id, turn).await,
        };
        Ok(self.schedule_automatic_research_turn(
            user_id.to_owned(),
            conversation_id.to_owned(),
            turn,
            clarification,
            model_access,
        ))
    }

    async fn reconcile_terminal_automatic_turn(
        &self,
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
        self.state
            .catalog
            .update_research_turn_status(&turn.turn_id, status, None, None, now())
            .map_err(map_catalog_error)?;
        let updated = self
            .state
            .catalog
            .owned_research_turn(user_id, conversation_id, &turn.turn_id)
            .map_err(map_catalog_error)?;
        Ok((updated, clarification))
    }

    fn begin_durable<T: Serialize>(
        &self,
        user_id: &str,
        resource_scope: &str,
        idempotency_key: Option<&str>,
        request: &T,
        serialization_key: Option<&str>,
    ) -> Result<ServiceClaimStart, PublicHttpError> {
        let key = idempotency_key.ok_or_else(|| {
            PublicHttpError::bounded_bad_request(
                "idempotency_key_required",
                "Idempotency-Key is required",
            )
        })?;
        if !(8..=128).contains(&key.len())
            || !key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(PublicHttpError::bounded_bad_request(
                "invalid_idempotency_key",
                "Idempotency-Key is invalid",
            ));
        }
        let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(request).map_err(PublicHttpError::internal_failure)?));
        let now = now();
        match self
            .state
            .catalog
            .claim_operation(NewDurableIdempotencyClaim {
                user_id,
                method: "POST",
                resource_scope,
                key,
                request_hash: &request_hash,
                serialization_key,
                now,
                expires_at: now + IDEMPOTENCY_RETENTION_SECONDS,
            })
            .map_err(map_catalog_error)?
        {
            DurableIdempotencyClaim::Claimed(operation) => {
                Ok(ServiceClaimStart::Claimed(DurableIdempotencyLease {
                    state: Arc::clone(&self.state),
                    user_id: user_id.to_owned(),
                    resource_scope: resource_scope.to_owned(),
                    key: key.to_owned(),
                    operation_id: operation.operation_id,
                    operation_created_at: operation.operation_created_at,
                    claim_token: operation.claim_token,
                    completed: false,
                }))
            }
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
                Ok(ServiceClaimStart::Replay(ServiceReplay {
                    status,
                    response_json,
                }))
            }
        }
    }

    fn finish_error(
        &self,
        lease: &mut DurableIdempotencyLease,
        error: PublicHttpError,
    ) -> Result<PublicHttpError, PublicHttpError> {
        let response_json = serde_json::to_string(&crate::ErrorResponse {
            code: error.code,
            message: error.public_message,
            retryable: error.retryable,
        })
        .map_err(PublicHttpError::internal_failure)?;
        self.state
            .catalog
            .complete_durable_idempotency(
                durable_completion_with_status(lease, error.status),
                &response_json,
            )
            .map_err(map_catalog_error)?;
        lease.completed = true;
        Ok(error)
    }
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

fn now() -> i64 {
    Utc::now().timestamp()
}

fn durable_completion_with_status(
    lease: &DurableIdempotencyLease,
    status: StatusCode,
) -> DurableIdempotencyCompletion<'_> {
    DurableIdempotencyCompletion {
        user_id: &lease.user_id,
        method: "POST",
        resource_scope: &lease.resource_scope,
        key: &lease.key,
        operation_id: &lease.operation_id,
        operation_created_at: lease.operation_created_at,
        claim_token: &lease.claim_token,
        status_code: i64::from(status.as_u16()),
    }
}

fn durable_completion(lease: &DurableIdempotencyLease) -> DurableIdempotencyCompletion<'_> {
    DurableIdempotencyCompletion {
        user_id: &lease.user_id,
        method: "POST",
        resource_scope: &lease.resource_scope,
        key: &lease.key,
        operation_id: &lease.operation_id,
        operation_created_at: lease.operation_created_at,
        claim_token: &lease.claim_token,
        status_code: i64::from(StatusCode::OK.as_u16()),
    }
}

async fn compensate_clarification(state: &DemoHostState, clarification: &ClarificationState) {
    if let Err(error) = state
        .research_runtime
        .cancel_clarification(&clarification.clarification_id, clarification.revision)
        .await
    {
        tracing::error!(error = %error, "failed to compensate clarification");
    }
}

fn model_access_from_profile(
    state: &DemoHostState,
    profile: &crate::catalog::ModelProfileRecord,
) -> Result<ModelAccessConfig, PublicHttpError> {
    let api_key = state
        .credential_cipher
        .decrypt(&profile.user_id, &profile.profile_id, &profile.encrypted_api_key)
        .map_err(PublicHttpError::internal_failure)?;
    if state.allow_private_model_endpoints {
        ModelAccessConfig::new(&profile.api_base_url, api_key, &profile.model_id)
    } else {
        ModelAccessConfig::new_public(&profile.api_base_url, api_key, &profile.model_id)
    }
    .map_err(PublicHttpError::internal_failure)
}

pub(crate) fn project_conversation_summary(
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
) -> Result<ResearchTurnResponse, CatalogError> {
    let has_answer = turn.answer_json.is_some();
    if (turn.status == ResearchTurnStatus::Completed && !has_answer)
        || (turn.status != ResearchTurnStatus::Completed && has_answer)
        || (turn.status == ResearchTurnStatus::Clarifying && clarification.is_none())
    {
        return Err(CatalogError::InvalidData("invalid research turn projection"));
    }
    let answer = turn
        .answer_json
        .as_deref()
        .map(serde_json::from_str::<ResearchAnswerResponse>)
        .transpose()
        .map_err(|_| CatalogError::InvalidData("invalid research answer"))?
        .map(|answer| project_chat_research_answer(&answer));
    let dialogue = clarification.map(|value| project_dialogue(value, turn.status));
    Ok(ResearchTurnResponse {
        turn_id: turn.turn_id,
        turn_number: turn.turn_number,
        user_question: turn.user_question,
        status: turn.status.as_str().to_owned(),
        answer,
        dialogue,
        created_at: turn.created_at,
        updated_at: turn.updated_at,
        completed_at: turn.completed_at,
    })
}

fn project_dialogue(
    clarification: ClarificationState,
    turn_status: ResearchTurnStatus,
) -> TurnDialogueResponse {
    let (status, failure) = dialogue_status_and_failure(turn_status, &clarification);
    TurnDialogueResponse {
        revision: clarification.revision,
        status: status.to_owned(),
        messages: clarification.dialogue,
        failure,
    }
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
                    .unwrap_or_else(|| "The model could not continue this question.".into()),
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

pub(crate) fn clarification_catalog_status(
    clarification: &ClarificationState,
) -> ResearchTurnStatus {
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

fn derive_operation_resource_id(operation_id: &str, purpose: &str) -> String {
    let mut value = format!("{:x}", Sha256::digest(format!("{operation_id}:{purpose}").as_bytes()));
    value.truncate(32);
    value
}

fn operation_datetime(seconds: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(seconds, 0).unwrap_or_else(Utc::now)
}

fn map_catalog_error(error: CatalogError) -> PublicHttpError {
    match error {
        CatalogError::NotFound => PublicHttpError::not_found(),
        CatalogError::Conflict(CatalogConflict::ConversationHasActiveTurn) => {
            PublicHttpError::conflict(
                "conversation_has_active_turn",
                "Finish or cancel the active turn before starting another",
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

fn map_dialogue_runtime_error(error: ResearchRuntimeError) -> PublicHttpError {
    match error {
        ResearchRuntimeError::Clarification(ClarificationError::StaleRevision { .. }) => {
            PublicHttpError::conflict(
                "dialogue_revision_conflict",
                "The dialogue changed; refresh before sending this message again",
            )
        }
        ResearchRuntimeError::Clarification(ClarificationError::InvalidTransition { .. }) => {
            PublicHttpError::conflict(
                "turn_not_accepting_messages",
                "This research turn is no longer accepting messages",
            )
        }
        other => PublicHttpError::internal_failure(other),
    }
}

fn map_create_turn_runtime_error(error: ResearchRuntimeError) -> PublicHttpError {
    match error {
        ResearchRuntimeError::Conversation(ConversationError::InvalidEvent(_)) => {
            PublicHttpError::conflict(
                "conversation_has_active_turn",
                "Finish or cancel the active turn before starting another",
            )
        }
        other => PublicHttpError::internal_failure(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_record(status: ResearchTurnStatus, answer_json: Option<String>) -> ResearchTurnRecord {
        ResearchTurnRecord {
            turn_id: "t".repeat(32),
            conversation_id: "c".repeat(32),
            turn_number: 1,
            clarification_id: "i".repeat(32),
            run_id: None,
            user_question: "question".into(),
            status,
            answer_style: ResearchAnswerStyle::WebFirst,
            model_profile_id: "p".repeat(32),
            model_profile_revision: 1,
            model_api_base_url: "https://model.example/v1/".into(),
            model_id: "model-id".into(),
            answer_json,
            created_at: 1,
            updated_at: 2,
            completed_at: None,
        }
    }

    #[test]
    fn operation_resource_ids_are_stable_and_scoped() {
        assert_eq!(
            derive_operation_resource_id("operation", "turn"),
            derive_operation_resource_id("operation", "turn")
        );
        assert_ne!(
            derive_operation_resource_id("operation", "turn"),
            derive_operation_resource_id("operation", "clarification")
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
        let mut turn = turn_record(
            ResearchTurnStatus::Completed,
            Some(complete_answer.to_string()),
        );
        turn.run_id = Some("r".repeat(32));
        turn.completed_at = Some(2);
        let response = project_research_turn(turn, None).unwrap();
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
    fn invalid_turn_state_combinations_are_rejected_by_the_service_projection() {
        let answer = serde_json::json!({
            "answer_style": "web_first",
            "answer": "Grounded answer",
            "knowledge_draft": {
                "answer": "draft",
                "claims": [],
                "uncertainty": "none",
                "basis_summary": "basis"
            },
            "comparison": {
                "agreements": [],
                "differences": [],
                "synthesis_rationale": "synthesis"
            },
            "claims": []
        })
        .to_string();

        for (turn, clarification) in [
            (turn_record(ResearchTurnStatus::Completed, None), None),
            (turn_record(ResearchTurnStatus::Running, Some(answer)), None),
            (turn_record(ResearchTurnStatus::Clarifying, None), None),
        ] {
            assert!(matches!(
                project_research_turn(turn, clarification),
                Err(CatalogError::InvalidData(_))
            ));
        }
    }

    #[test]
    fn automatic_failure_is_never_presented_as_research_started() {
        assert_eq!(
            terminal_dialogue_outcome(ResearchTurnStatus::Failed),
            Some(("failed", AUTOMATIC_RESEARCH_FAILURE_MESSAGE)),
        );
        assert_eq!(terminal_dialogue_outcome(ResearchTurnStatus::Running), None);
    }
}
