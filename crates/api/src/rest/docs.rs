//! OpenAPI document (utoipa) served at `/api/openapi.json` and browsable
//! at `/api/docs`. `setu openapi` writes these exact bytes to the
//! checked-in `openapi.json` at the workspace root; the snapshot test
//! enforces equality (`UPDATE=1` rewrites).

use utoipa::OpenApi;

use super::apps::{
    AppListOut, AppOut, AppVersionListOut, AppVersionOut, ManifestErrorOut, PublishRequest,
    PublishResponse, ValidateRequest, ValidateResponse,
};
use super::auth::{LoginRequest, OperatorOut};
use super::backups::{BackupJobResponse, BackupListOut, BackupOut};
use super::jobs::{AuditEntryOut, AuditListOut, AuditQuery, JobListOut, JobsQuery};
use super::operators::{CreateOperatorRequest, OperatorListOut};
use super::plans::{CreatePlanRequest, PlanLimitsIn, PlanListOut, PlanOut, UpdatePlanRequest};
use super::servers::{CreateServerRequest, CreateServerResponse, ServerListOut, ServerOut};
use super::setup::{LocalServerRequest, LocalServerResponse, SetupRequest, SetupStatusOut};
use super::sites::{
    CreateSiteRequest, CreateSiteResponse, EnvEntryOut, EnvGetOut, EnvPutRequest, JobOut,
    SiteJobResponse, SiteListOut, SiteOut, UpdateSiteRequest,
};
use super::{ErrorBody, ErrorDetail, HealthOut, ReadyOut};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Setu API",
        version = "0.1.0",
        description = "Control-plane REST API for single-binary web apps"
    ),
    paths(
        super::healthz,
        super::readyz,
        super::auth::login,
        super::auth::logout,
        super::auth::me,
        super::operators::list,
        super::operators::create,
        super::setup::status,
        super::setup::create_owner,
        super::setup::local_server,
        super::servers::list,
        super::servers::create,
        super::servers::get,
        super::servers::delete,
        super::apps::list,
        super::apps::validate,
        super::apps::publish,
        super::apps::versions,
        super::plans::list,
        super::plans::create,
        super::plans::get,
        super::plans::update,
        super::sites::list,
        super::sites::create,
        super::sites::get,
        super::sites::restart,
        super::sites::stop,
        super::sites::start,
        super::sites::update,
        super::sites::retry,
        super::sites::delete,
        super::sites::env_get,
        super::sites::env_put,
        super::sites::logs,
        super::backups::list,
        super::backups::create,
        super::backups::restore,
        super::jobs::list,
        super::jobs::get,
        super::jobs::log,
        super::jobs::audit_list,
        super::agent::join,
        super::agent::commands,
        super::agent::result,
        super::agent::heartbeat,
    ),
    components(schemas(
        HealthOut,
        ReadyOut,
        ErrorBody,
        ErrorDetail,
        LoginRequest,
        OperatorOut,
        OperatorListOut,
        CreateOperatorRequest,
        ServerOut,
        ServerListOut,
        CreateServerRequest,
        CreateServerResponse,
        SetupStatusOut,
        SetupRequest,
        LocalServerRequest,
        LocalServerResponse,
        AppOut,
        AppListOut,
        AppVersionOut,
        AppVersionListOut,
        ValidateRequest,
        ValidateResponse,
        ManifestErrorOut,
        PublishRequest,
        PublishResponse,
        PlanOut,
        PlanListOut,
        PlanLimitsIn,
        CreatePlanRequest,
        UpdatePlanRequest,
        SiteOut,
        SiteListOut,
        JobOut,
        CreateSiteRequest,
        CreateSiteResponse,
        UpdateSiteRequest,
        SiteJobResponse,
        EnvEntryOut,
        EnvGetOut,
        EnvPutRequest,
        BackupOut,
        BackupListOut,
        BackupJobResponse,
        JobListOut,
        JobsQuery,
        AuditEntryOut,
        AuditListOut,
        AuditQuery,
        setu_proto::JoinRequest,
        setu_proto::JoinResponse,
        setu_proto::Command,
        setu_proto::CommandEnvelope,
        setu_proto::CommandResult,
        setu_proto::Heartbeat,
        setu_proto::SiteSpec,
        setu_proto::SiteMetric,
        setu_proto::UnitState,
        setu_proto::EnvVar,
        setu_proto::Limits,
        setu_proto::BinaryRef,
    ))
)]
pub struct ApiDoc;

/// The canonical document bytes: pretty JSON plus one trailing newline.
/// Both `setu openapi` and the snapshot test use this function so the
/// checked-in file and the served document can never drift.
pub fn openapi_json_string() -> String {
    format!(
        "{}\n",
        ApiDoc::openapi()
            .to_pretty_json()
            .expect("openapi serialises")
    )
}
