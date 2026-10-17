use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct DeployManager {
    current_session: Option<DeploySession>,
}

struct DeploySession {
    dest_path: PathBuf,
    tmp_path: PathBuf,
    file: Option<File>,
    hasher: Sha256,
    expected_size: u64,
    expected_sha256: String,
    written_bytes: u64,
    next_chunk_idx: u32,
    finished_successfully: bool,
}

impl Drop for DeploySession {
    fn drop(&mut self) {
        if !self.finished_successfully && self.tmp_path.exists() {
            let _ = fs::remove_file(&self.tmp_path);
        }
    }
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
        } else if cfg!(debug_assertions) || std::env::var("DEVBRIDGE_MOCK_ROMS").is_ok() {
            let tmp_roms = std::env::temp_dir().join("r36s-roms-mock");
            fs::create_dir_all(&tmp_roms).map_err(|e| e.to_string())?;
            tmp_roms
        } else {
            return Err("Partição de ROMs (/roms ou /roms2) não encontrada no console".to_string());
        };

        let canon_base = base_dir.canonicalize().map_err(|e| {
            format!(
                "Falha ao resolver caminho base {}: {}",
                base_dir.display(),
                e
            )
        })?;

        // Sanitização rigorosa de caminho para prevenir path traversal
        let clean_rel: PathBuf = relative_path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".." && *p != ".")
            .collect();

        let clean_filename = Path::new(filename)
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| "Nome de arquivo inválido".to_string())?;

        let target_dir = base_dir.join(clean_rel);
        fs::create_dir_all(&target_dir)
            .map_err(|e| format!("Falha ao criar diretório {}: {}", target_dir.display(), e))?;

        let canon_target = target_dir
            .canonicalize()
            .map_err(|e| format!("Falha ao validar destino {}: {}", target_dir.display(), e))?;

        if !canon_target.starts_with(&canon_base) {
            return Err(
                "Violação de segurança: tentativa de gravação fora do diretório de ROMs"
                    .to_string(),
            );
        }

        let dest_path = canon_target.join(clean_filename);
        let pid = std::process::id();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let tmp_path =
            canon_target.join(format!(".{}.{}_{}.upload.tmp", clean_filename, pid, now_ms));

        let file = File::create(&tmp_path)
            .map_err(|e| format!("Falha ao criar arquivo temporário: {}", e))?;

        self.current_session = Some(DeploySession {
            dest_path,
            tmp_path,
            file: Some(file),
            hasher: Sha256::new(),
            expected_size: total_size,
            expected_sha256: sha256.to_lowercase(),
            written_bytes: 0,
            next_chunk_idx: 0,
            finished_successfully: false,
        });

        Ok(format!("Iniciado deploy para {}", filename))
    }

    pub fn append_chunk(&mut self, chunk_idx: u32, data_bytes: &[u8]) -> Result<u64, String> {
        let session = self
            .current_session
            .as_mut()
            .ok_or_else(|| "Nenhuma sessão de deploy ativa".to_string())?;

        if chunk_idx != session.next_chunk_idx {
            return Err(format!(
                "Ordem de chunk incorreta: recebido {}, esperado {}",
                chunk_idx, session.next_chunk_idx
            ));
        }

        if session.written_bytes + (data_bytes.len() as u64) > session.expected_size {
            return Err(format!(
                "Tamanho excedido: esperado {} bytes, tentativa de gravar {} bytes adicionais",
                session.expected_size,
                session.written_bytes + data_bytes.len() as u64
            ));
        }

        let file = session
            .file
            .as_mut()
            .ok_or_else(|| "Arquivo de deploy não está aberto para escrita".to_string())?;

        file.write_all(data_bytes)
            .map_err(|e| format!("Erro de escrita em disco: {}", e))?;

        session.hasher.update(data_bytes);
        session.written_bytes += data_bytes.len() as u64;
        session.next_chunk_idx += 1;

        Ok(session.written_bytes)
    }

    pub fn finish(&mut self) -> Result<(String, String), String> {
        let mut session = self
            .current_session
            .take()
            .ok_or_else(|| "Nenhuma sessão de deploy ativa".to_string())?;

        if session.written_bytes != session.expected_size {
            return Err(format!(
                "Tamanho incompleto: recebidos {} de {} bytes esperados",
                session.written_bytes, session.expected_size
            ));
        }

        if let Some(mut file) = session.file.take() {
            file.flush()
                .map_err(|e| format!("Erro no flush em disco: {}", e))?;
            file.sync_all()
                .map_err(|e| format!("Erro no sync_all em disco: {}", e))?;
            drop(file);
        }

        let final_hash = format!("{:x}", session.hasher.clone().finalize());
        if session.expected_sha256.is_empty() || final_hash != session.expected_sha256 {
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
                    let _ =
                        fs::set_permissions(&session.tmp_path, fs::Permissions::from_mode(0o755));
                }
            }
        }

        // Renomeia atomicamente
        fs::rename(&session.tmp_path, &session.dest_path)
            .map_err(|e| format!("Erro ao finalizar arquivo: {}", e))?;

        // Sincroniza o diretório pai para persistência durável no filesystem
        if let Some(parent) = session.dest_path.parent() {
            if let Ok(dir_file) = File::open(parent) {
                let _ = dir_file.sync_all();
            }
        }

        session.finished_successfully = true;
        Ok((session.dest_path.to_string_lossy().to_string(), final_hash))
    }
}
