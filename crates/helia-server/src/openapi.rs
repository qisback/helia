// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `OpenAPI` document generated from the handler annotations.

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::error::{ErrorBody, ErrorEnvelope};
use crate::jobs::{
    AutoApSummary, BackendArg, CalibrationConfig, ChannelArg, ChannelSummary, DebayerArg,
    JobConfig, JobEvent, JobRequest, JobState, JobSummary, JobView, LimbSummary, MetricArg, Region,
    RobustArg, SharpenCheckSummary, StageTiming, TrueColourSummary,
};
use crate::routes;

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// The API description. Routes are generated from `#[utoipa::path]`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Helia API",
        description = "Solar lucky-imaging stacker. All paths are relative to the server's data root.",
        license(name = "GPL-3.0-or-later")
    ),
    paths(
        routes::health,
        routes::version,
        routes::presets,
        routes::preset_lut,
        routes::inspect,
        routes::frame,
        routes::create_job,
        routes::list_jobs,
        routes::get_job,
        routes::delete_job,
        routes::job_result,
        routes::job_preview,
        routes::job_sunspots_csv,
        routes::job_sunspots_png,
        routes::job_report,
        routes::job_report_file,
        routes::job_events,
        crate::sequences::timelapse,
        crate::sequences::solar_derotation,
        crate::sequences::mosaic,
        routes::openapi_json,
        routes::docs,
    ),
    components(schemas(
        ErrorBody,
        ErrorEnvelope,
        JobConfig,
        CalibrationConfig,
        JobRequest,
        JobView,
        JobState,
        JobSummary,
        AutoApSummary,
        LimbSummary,
        SharpenCheckSummary,
        TrueColourSummary,
        ChannelSummary,
        JobEvent,
        StageTiming,
        BackendArg,
        RobustArg,
        ChannelArg,
        DebayerArg,
        MetricArg,
        Region,
        crate::presets::PresetList,
        crate::presets::PresetView,
        crate::presets::PresetColour,
        crate::presets::PresetSharpen,
        routes::Recommended,
        routes::Health,
        routes::VersionInfo,
        routes::BuildDetails,
        routes::LicenceDetails,
        routes::CaptureInfo,
        routes::SidecarInfo,
        routes::SourcedText,
        routes::SourcedNumber,
        routes::SourcedFlag,
        routes::EffectiveInfo,
        routes::SerHeaderInfo,
        routes::TimestampInfo,
        routes::DetectionInfo,
        routes::NoticeInfo,
        routes::InspectRequest,
        crate::sequences::TimelapseRequest,
        crate::sequences::TimelapseResponse,
        crate::sequences::TimelapseFrame,
        crate::sequences::DerotateConfig,
        crate::sequences::SolarDerotationRequest,
        crate::sequences::SolarDerotationResponse,
        crate::sequences::DerotatedImage,
        crate::sequences::MosaicRequest,
        crate::sequences::MosaicResponse,
        crate::sequences::MosaicPanel,
        crate::sequences::MosaicPair,
    )),
    modifiers(&BearerAuth),
    security(("bearer" = []))
)]
pub struct ApiDoc;

/// The document as pretty JSON.
///
/// # Errors
/// Serialisation failure (not expected).
pub fn document() -> Result<String, serde_json::Error> {
    ApiDoc::openapi().to_pretty_json()
}
