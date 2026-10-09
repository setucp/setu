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
use super::operators::{CreateOperatorRequest, OperatorListOut};
use super::plans::{CreatePlanRequest, PlanLimitsIn, PlanListOut, PlanOut, UpdatePlanRequest};
use super::servers::{CreateServerRequest, CreateServerResponse, ServerListOut, ServerOut};
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
