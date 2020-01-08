mod http;
mod manifest;
mod err;
mod auth;
mod chunks;
mod spool;

use std::sync::{Arc, Mutex};
pub use err::WickResult;
use std::io::{Seek, SeekFrom, Cursor};
use tokio::io::{AsyncReadExt};
use john_wick_parse::assets::Newable;
use john_wick_parse::archives::{FPakInfo, FPakIndex};
use block_modes::{BlockMode, Ecb, block_padding::ZeroPadding};
use aes_soft::Aes256;

const PAK_SIZE: u32 = 8 + 16 + 20 + 1 + 16 + (32 * 5);

pub struct ServiceState {
    http: Arc<crate::http::HttpService>,
    app_manifest: manifest::AppManifest,
    chunk_manifest: manifest::ChunkManifest,
    files: Vec<manifest::ChunkManifestFile>,
}

pub struct PakService {
    pak_index: FPakIndex,
    reader: Mutex<chunks::ChunkReader>,
}

impl ServiceState {
    pub async fn new() -> WickResult<Self> {
        let http_service = Arc::new(crate::http::HttpService::new());
        let access_token = auth::get_token(&http_service).await?;
        let app_manifest = manifest::get_manifest(&http_service, &access_token).await?;
        let mut chunk_manifest = manifest::get_chunk_manifest(&http_service, &app_manifest).await?;
        
        // Filter out just the pak files
        let files = chunk_manifest.get_files_mut(|v| &v[v.len() - 4..] == ".pak" && &v[..8] == "Fortnite");

        Ok(Self {
            http: http_service,
            app_manifest,
            chunk_manifest,
            files,
        })
    }

    pub fn get_paks(&self) -> Vec<String> {
        self.files.iter().map(|v| v.filename.to_owned()).collect()
    } 

    pub async fn get_pak(&self, file: String, key: String) -> WickResult<PakService> {
        let file = match self.files.iter().find(|v| v.filename == file) {
            Some(f) => f,
            None => return err::make_err("File does not exist"),
        };

        let mut reader = chunks::make_reader(self.http.clone(), &self.chunk_manifest, &self.app_manifest, &file)?;

        // Read pak header
        reader.seek(SeekFrom::End(-(PAK_SIZE as i64)))?;
        let mut header = [0u8; PAK_SIZE as usize];
        reader.read_exact(&mut header).await?;
        let mut header_cursor = Cursor::new(&header[..]);
        let pak_header = FPakInfo::new(&mut header_cursor)?;

        // Retrieve and decrypt index
        let (index_start, index_length) = pak_header.get_index_sizes();
        reader.seek(SeekFrom::Start(index_start))?;
        let mut buffer = vec![0u8; index_length as usize];
        reader.read_exact(&mut buffer).await?;

        let key = hex::decode(&key)?;
        let decrypt = Ecb::<Aes256, ZeroPadding>::new_var(&key, Default::default())?;
        decrypt.decrypt(&mut buffer)?;

        let mut index_cursor = Cursor::new(buffer.as_slice());
        let pak_index = FPakIndex::new(&mut index_cursor)?;

        Ok(PakService {
            pak_index,
            reader: Mutex::new(reader),
        })
    }
}

impl PakService {
    pub fn get_files(&self) -> Vec<String> {
        self.pak_index.get_entries().iter().map(|v| v.get_filename().to_owned()).collect()
    }

    pub fn get_mount_point(&self) -> &str {
        &self.pak_index.get_mount_point()
    }

    pub async fn get_data(&self, filename: &str) -> WickResult<Vec<u8>> {
        let file = match self.pak_index.get_entries().iter().find(|v| v.get_filename() == filename) {
            Some(f) => f,
            None => return err::make_err("Could not find file"),
        };

        let mut reader = self.reader.lock().unwrap().reset();

        let start_pos = file.position as u64 + file.struct_size;
        reader.seek(SeekFrom::Start(start_pos))?;
        let mut buffer = vec![0u8; file.size as usize];
        reader.read_exact(&mut buffer).await?;

        Ok(buffer)
    }
}