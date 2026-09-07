use lofty::{
    prelude::{Accessor, TaggedFileExt},
    probe::Probe,
    tag::ItemKey,
};
use music_folder_core::{ports::MetadataReader, TrackMetadata};
use std::{borrow::Cow, path::Path};

pub struct LoftyMetadataReader;
impl MetadataReader for LoftyMetadataReader {
    fn read(&self, path: &Path) -> Result<TrackMetadata, String> {
        let tagged = Probe::open(path)
            .map_err(|e| e.to_string())?
            .read()
            .map_err(|e| e.to_string())?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        Ok(TrackMetadata {
            artist: tag.and_then(|t| t.artist()).map(Cow::into_owned),
            album_artist: tag
                .and_then(|t| t.get_string(&ItemKey::AlbumArtist))
                .map(str::to_owned),
            album: tag.and_then(|t| t.album()).map(Cow::into_owned),
            title: tag.and_then(|t| t.title()).map(Cow::into_owned),
            track_no: tag.and_then(|t| t.track()),
            disc_no: tag.and_then(|t| t.disk()),
            year: tag.and_then(|t| t.year()).map(|v| v as i32),
        })
    }
}
