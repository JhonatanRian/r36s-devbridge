use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct DeployManager {
    current_session: Option<DeploySession>,
}

struct DeploySession {
    dest_path: PathBuf,
    tmp_path: PathBuf,
    file: File,
    hasher: Sha256,
    expected_size: u64,
    expected_sha256: String,
    written_bytes: u64,
}

impl DeployManager {
    pub fn new() -> Self {
        Self {
            current_session: None,
        }
    }

    pub fn start(
        &mut self,
        relative_path: &str,
        filename: &str,
        total_size: u64,
        sha256: &str,
    ) -> Result<String, String> {
        // Base padrão de ROMs no ArkOS do R36S
        let base_dir = if Path::new("/roms").exists() {
            PathBuf::from("/roms")
        } else if Path::new("/roms2").exists() {
            PathBuf::from("/roms2")
        } else {
            PathBuf::from("/tmp/r36s-deploy")
        };

        // Sanitização de caminho para segurança
        let clean_rel: PathBuf = relative_path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".." && *p != ".")
            .collect();

        let clean_filename = Path::new(filename)
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| "Nome de arquivo inválido".to_string())?;

        let target_dir = base_dir.join(clean_rel);
        fs::create_dir_all(&target_dir).map_err(|e| format!("Falha ao criar diretório: {}", e))?;

        let dest_path = target_dir.join(clean_filename);
        let tmp_path = target_dir.join(format!(".{}.upload.tmp", clean_filename));

        let file = File::create(&tmp_path).map_err(|e| format!("Falha ao criar arquivo temporário: {}", e))?;

        self.current_session = Some(DeploySession {
            dest_path,
            tmp_path,
            file,
            hasher: Sha256::new(),
            expected_size: total_size,
            expected_sha256: sha256.to_lowercase(),
            written_bytes: 0,
        });

        Ok(format!("Iniciado deploy para {}", filename))
    }

    pub fn append_chunk(&mut self, _chunk_idx: u32, data_bytes: &[u8]) -> Result<u64, String> {
        let session = self
            .current_session
            .as_mut()
            .ok_or_else(|| "Nenhuma sessão de deploy ativa".to_string())?;

        session
            .file
            .write_all(data_bytes)
            .map_err(|e| format!("Erro de escrita em disco: {}", e))?;

        session.hasher.update(data_bytes);
        session.written_bytes += data_bytes.len() as u64;

        Ok(session.written_bytes)
    }

    pub fn finish(&mut self) -> Result<(String, String), String> {
        let mut session = self
            .current_session
            .take()
            .ok_or_else(|| "Nenhuma sessão de deploy ativa".to_string())?;

        session.file.flush().map_err(|e| format!("Erro ao sincronizar disco: {}", e))?;
        drop(session.file);

        let final_hash = format!("{:x}", session.hasher.finalize());
        if !session.expected_sha256.is_empty() && final_hash != session.expected_sha256 {
            let _ = fs::remove_file(&session.tmp_path);
            return Err(format!(
                "Checksum divergente! Esperado: {}, Obtido: {}",
                session.expected_sha256, final_hash
            ));
        }

        // Dá permissão executável se for script .sh
        if let Some(ext) = session.dest_path.extension() {
            if ext == "sh" {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&session.tmp_path, fs::Permissions::from_mode(0o755));
                }
            }
        }

        // Renomeia atomicamente
        fs::rename(&session.tmp_path, &session.dest_path)
            .map_err(|e| format!("Erro ao finalizar arquivo: {}", e))?;

        Ok((session.dest_path.to_string_lossy().to_string(), final_hash))
    }
}
