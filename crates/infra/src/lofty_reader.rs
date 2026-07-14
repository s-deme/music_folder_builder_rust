use lofty::{
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
};
use music_folder_core::{ports::MetadataReader, TrackMetadata};
use std::path::Path;

pub struct LoftyMetadataReader;
impl MetadataReader for LoftyMetadataReader {
    fn read(&self, path: &Path) -> Result<TrackMetadata, String> {
        let tagged = Probe::open(path)
            .map_err(|e| e.to_string())?
            .read()
            .map_err(|e| e.to_string())?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        Ok(TrackMetadata {
            artist: tag.and_then(|t| t.artist()).map(|value| value.into_owned()),
            // Album artist is format-dependent. It is intentionally left empty until
            // the accepted lofty version has passed the M4A/OGG fixture suite.
            album_artist: None,
            album: tag.and_then(|t| t.album()).map(|value| value.into_owned()),
            title: tag.and_then(|t| t.title()).map(|value| value.into_owned()),
            track_no: tag.and_then(|t| t.track()),
            disc_no: tag.and_then(|t| t.disk()),
            year: tag.and_then(|t| t.year()).map(|v| v as i32),
        })
    }
}
