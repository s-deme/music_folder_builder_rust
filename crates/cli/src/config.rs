use crate::{args::PlanCreateArgs, output::CliFailure};
use music_folder_core::{DuplicateStrategy, NamingRules};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PlanFileConfig {
    naming: NamingFileConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct NamingFileConfig {
    artist_dir_template: Option<String>,
    album_dir_template: Option<String>,
    disc_dir_template: Option<String>,
    filename_template: Option<String>,
    duplicate_suffix_template: Option<String>,
    duplicate_strategy: Option<String>,
    use_source_filename: Option<bool>,
    use_source_image_filename: Option<bool>,
    allow_missing_metadata: Option<bool>,
    allow_long_paths: Option<bool>,
}

pub fn load_naming(args: &PlanCreateArgs) -> Result<NamingRules, CliFailure> {
    let file = match &args.config {
        Some(path) => {
            let metadata = std::fs::metadata(path).map_err(|error| {
                CliFailure::usage(
                    "config_read_failed",
                    format!("failed to inspect {}: {error}", path.display()),
                )
            })?;
            if metadata.len() > 65_536 {
                return Err(CliFailure::usage(
                    "config_too_large",
                    "naming TOML must not exceed 65536 bytes",
                ));
            }
            let source = std::fs::read_to_string(path).map_err(|error| {
                CliFailure::usage(
                    "config_read_failed",
                    format!("failed to read {}: {error}", path.display()),
                )
            })?;
            toml::from_str::<PlanFileConfig>(&source).map_err(|error| {
                CliFailure::usage(
                    "invalid_config",
                    format!("invalid TOML in {}: {error}", path.display()),
                )
            })?
        }
        None => PlanFileConfig::default(),
    };

    let mut naming = NamingRules::default();
    if let Some(value) = file.naming.artist_dir_template {
        naming.artist_dir_template = value;
    }
    if let Some(value) = file.naming.album_dir_template {
        naming.album_dir_template = value;
    }
    if let Some(value) = file.naming.disc_dir_template {
        naming.disc_dir_template = value;
    }
    if let Some(value) = file.naming.filename_template {
        naming.filename_template = value;
    }
    if let Some(value) = file.naming.duplicate_suffix_template {
        naming.duplicate_suffix_template = value;
    }
    if let Some(value) = file.naming.duplicate_strategy {
        naming.duplicate_strategy = parse_duplicate_strategy(&value)?;
    }
    if let Some(value) = file.naming.use_source_filename {
        naming.use_source_filename = value;
    }
    if let Some(value) = file.naming.use_source_image_filename {
        naming.use_source_image_filename = value;
    }
    if let Some(value) = file.naming.allow_missing_metadata {
        naming.allow_missing_metadata = value;
    }
    if let Some(value) = file.naming.allow_long_paths {
        naming.allow_long_paths = value;
    }

    // Explicit CLI flags have priority over the TOML values.
    if let Some(value) = &args.artist_dir_template {
        naming.artist_dir_template.clone_from(value);
    }
    if let Some(value) = &args.album_dir_template {
        naming.album_dir_template.clone_from(value);
    }
    if let Some(value) = &args.disc_dir_template {
        naming.disc_dir_template.clone_from(value);
    }
    if let Some(value) = &args.filename_template {
        naming.filename_template.clone_from(value);
    }
    if let Some(value) = &args.duplicate_suffix_template {
        naming.duplicate_suffix_template.clone_from(value);
    }
    if let Some(value) = &args.duplicate_strategy {
        naming.duplicate_strategy = (*value).into();
    }
    if args.use_source_filename {
        naming.use_source_filename = true;
    }
    if args.use_source_image_filename {
        naming.use_source_image_filename = true;
    }
    if args.allow_missing_metadata {
        naming.allow_missing_metadata = true;
    }
    if args.allow_long_paths {
        naming.allow_long_paths = true;
    }
    for (field, value) in [
        ("artist_dir_template", naming.artist_dir_template.as_str()),
        ("album_dir_template", naming.album_dir_template.as_str()),
        ("disc_dir_template", naming.disc_dir_template.as_str()),
        ("filename_template", naming.filename_template.as_str()),
        (
            "duplicate_suffix_template",
            naming.duplicate_suffix_template.as_str(),
        ),
    ] {
        if value.len() > 1_024 {
            return Err(CliFailure::usage(
                "naming_template_too_large",
                format!("{field} must not exceed 1024 UTF-8 bytes"),
            ));
        }
    }
    Ok(naming)
}

fn parse_duplicate_strategy(value: &str) -> Result<DuplicateStrategy, CliFailure> {
    match value {
        "skip" => Ok(DuplicateStrategy::Skip),
        "sequence" => Ok(DuplicateStrategy::Sequence),
        "template" => Ok(DuplicateStrategy::Template),
        _ => Err(CliFailure::usage(
            "invalid_duplicate_strategy",
            "duplicate_strategy must be skip, sequence, or template",
        )),
    }
}
