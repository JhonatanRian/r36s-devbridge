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
    backup_path: Option<PathBuf>,
    dest_existed: bool,
    file: Option<File>,
    hasher: Sha256,
    expected_size: u64,
    expected_sha256: String,
    written_bytes: u64,
    next_chunk_idx: u32,
    finished_successfully: bool,
    is_executable: bool,
    is_sandbox: bool,
}

#[cfg(unix)]
fn is_mount_noexec(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };

    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            (stat.f_flag & libc::ST_NOEXEC as libc::c_ulong) != 0
        } else {
            false
        }
    }
}

#[cfg(not(unix))]
fn is_mount_noexec(_path: &Path) -> bool {
    false
}

impl Drop for DeploySession {
    fn drop(&mut self) {
        if !self.finished_successfully {
            if self.tmp_path.exists() {
                let _ = fs::remove_file(&self.tmp_path);
            }
            if let Some(ref bp) = self.backup_path {
                if bp.exists() {
                    let _ = fs::rename(bp, &self.dest_path);
                }
            }
        } else if let Some(ref bp) = self.backup_path {
            if bp.exists() {
                let _ = fs::remove_file(bp);
            }
        }
    }
}

impl DeployManager {
    pub fn new() -> Self {
        Self {
            current_session: None,
        }
    }

    pub fn resolve_and_validate_path(
        relative_path: &str,
        filename: &str,
    ) -> Result<(PathBuf, PathBuf, bool), String> {
        if relative_path.contains("..") || filename.contains("..") {
            return Err("Violação de segurança: '..' não permitido no caminho de destino".to_string());
        }

        let is_sandbox = relative_path == "@sandbox"
            || relative_path.starts_with("@sandbox/")
            || relative_path.starts_with("@sandbox\\");

        let (target_dir, canon_base, is_sb) = if is_sandbox {
            let sb_dir = crate::process::resolve_sandbox_executable_path(relative_path)?;
            fs::create_dir_all(&sb_dir)
                .map_err(|e| format!("Falha ao criar diretório sandbox: {}", e))?;
            let canon = sb_dir.canonicalize().map_err(|e| {
                format!(
                    "Falha ao resolver caminho base sandbox {}: {}",
                    sb_dir.display(),
                    e
                )
            })?;
            (sb_dir, canon, true)
        } else {
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
            let clean_rel: PathBuf = relative_path
                .split(['/', '\\'])
                .filter(|p| !p.is_empty() && *p != ".." && *p != ".")
                .collect();
            let target = base_dir.join(clean_rel);
            fs::create_dir_all(&target)
                .map_err(|e| format!("Falha ao criar diretório {}: {}", target.display(), e))?;
            (target, canon_base, false)
        };

        let canon_target = target_dir
            .canonicalize()
            .map_err(|e| format!("Falha ao validar destino {}: {}", target_dir.display(), e))?;

        if !is_sb && !canon_target.starts_with(&canon_base) {
            return Err(
                "Violação de segurança: tentativa de gravação fora do diretório autorizado"
                    .to_string(),
            );
        }

        let clean_filename = Path::new(filename)
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| "Nome de arquivo inválido".to_string())?;

        let dest_path = canon_target.join(clean_filename);
        Ok((canon_target, dest_path, is_sb))
    }

    pub fn start(
        &mut self,
        relative_path: &str,
        filename: &str,
        total_size: u64,
        sha256: &str,
        executable: bool,
    ) -> Result<String, String> {
        let (canon_target, dest_path, is_sandbox) =
            Self::resolve_and_validate_path(relative_path, filename)?;

        let pid = std::process::id();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let clean_filename = dest_path.file_name().and_then(|f| f.to_str()).unwrap_or("file");
        let dest_existed = dest_path.exists();
        let backup_path = if dest_existed {
            Some(canon_target.join(format!(".{}.{}_{}.backup.tmp", clean_filename, pid, now_ms)))
        } else {
            None
        };

        let tmp_path =
            canon_target.join(format!(".{}.{}_{}.upload.tmp", clean_filename, pid, now_ms));

        let file = File::create(&tmp_path)
            .map_err(|e| format!("Falha ao criar arquivo temporário: {}", e))?;

        self.current_session = Some(DeploySession {
            dest_path,
            tmp_path,
            backup_path,
            dest_existed,
            file: Some(file),
            hasher: Sha256::new(),
            expected_size: total_size,
            expected_sha256: sha256.to_lowercase(),
            written_bytes: 0,
            next_chunk_idx: 0,
            finished_successfully: false,
            is_executable: executable,
            is_sandbox,
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

        let ext_is_sh = session
            .dest_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext == "sh")
            .unwrap_or(false);

        let should_exec = session.is_executable || ext_is_sh || session.is_sandbox;
        if should_exec {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    fs::set_permissions(&session.tmp_path, fs::Permissions::from_mode(0o755));
            }
        }

        // Se o arquivo de destino já existe, renomeia atomicamente para backup antes de promover o upload
        if session.dest_existed && session.dest_path.exists() {
            if let Some(ref bp) = session.backup_path {
                fs::rename(&session.dest_path, bp)
                    .map_err(|e| format!("Erro ao mover arquivo existente para backup temporário: {}", e))?;
            }
        }

        // Renomeia o upload temporário para o destino
        if let Err(e) = fs::rename(&session.tmp_path, &session.dest_path) {
            if let Some(ref bp) = session.backup_path {
                if bp.exists() {
                    let _ = fs::rename(bp, &session.dest_path);
                }
            }
            return Err(format!("Erro ao finalizar arquivo via rename: {}", e));
        }

        if session.is_executable {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&session.dest_path, fs::Permissions::from_mode(0o755));

                if session.is_sandbox {
                    let meta_ok = match fs::metadata(&session.dest_path) {
                        Ok(meta) => (meta.permissions().mode() & 0o111) != 0,
                        Err(_) => false,
                    };
                    let noexec = is_mount_noexec(&session.dest_path);

                    if !meta_ok || noexec {
                        let _ = fs::remove_file(&session.dest_path);
                        if let Some(ref bp) = session.backup_path {
                            if bp.exists() {
                                let _ = fs::rename(bp, &session.dest_path);
                            }
                        }
                        return Err(
                            "O sistema de arquivos não permitiu permissão de execução (possível montagem noexec)"
                                .to_string(),
                        );
                    }
                } else {
                    if is_mount_noexec(&session.dest_path) {
                        let _ = fs::remove_file(&session.dest_path);
                        if let Some(ref bp) = session.backup_path {
                            if bp.exists() {
                                let _ = fs::rename(bp, &session.dest_path);
                            }
                        }
                        return Err(
                            "Partição montada com flag noexec, execução não permitida".to_string(),
                        );
                    }
                }
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deploy_sandbox_and_rollback_on_failure() {
        let mut mgr = DeployManager::new();
        let payload = b"hello r36s binary";
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let correct_hash = format!("{:x}", hasher.finalize());

        // 1. Deploy inicial com sucesso em @sandbox/
        mgr.start("@sandbox", "test_app", payload.len() as u64, &correct_hash, true)
            .expect("Início do deploy");
        mgr.append_chunk(0, payload).expect("Chunk de deploy");
        let (path, hash) = mgr.finish().expect("Finalização com sucesso");
        assert_eq!(hash, correct_hash);
        let dest = PathBuf::from(&path);
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), payload);

        // 2. Novo deploy sobre o mesmo arquivo, mas com falha de checksum -> deve restaurar o arquivo anterior!
        mgr.start("@sandbox", "test_app", payload.len() as u64, "badhash0000000000000000000000000000000000000000000000000000000000", true)
            .expect("Início do segundo deploy");
        mgr.append_chunk(0, b"corrupted payload").expect("Chunk corrompido");
        let fail_res = mgr.finish();
        assert!(fail_res.is_err(), "Deve falhar por checksum divergente");

        // O arquivo anterior DEVE ter sido preservado
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), payload, "Conteúdo anterior deve ser preservado após falha");

        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn test_deploy_path_traversal_rejection() {
        let mut mgr = DeployManager::new();
        let res = mgr.start("../../../etc", "passwd", 10, "dummy", false);
        assert!(res.is_err() || res.unwrap_err().contains("fora do diretório"));
    }

    #[test]
    fn test_resolve_and_validate_path_sandbox_and_mock_roms() {
        // 1. @sandbox/app
        let (target_dir, dest_path, is_sb) = DeployManager::resolve_and_validate_path("@sandbox/sub", "app.bin")
            .expect("Resolução de caminho sandbox");
        assert!(is_sb);
        assert!(target_dir.ends_with("sub"));
        assert!(dest_path.ends_with("app.bin"));

        // 2. Mock roms (via DEVBRIDGE_MOCK_ROMS)
        std::env::set_var("DEVBRIDGE_MOCK_ROMS", "1");
        let (roms_target, roms_dest, is_sb2) = DeployManager::resolve_and_validate_path("ports/test", "game.sh")
            .expect("Resolução de caminho ports em ROMs mock");
        assert!(!is_sb2);
        assert!(roms_target.ends_with("ports/test"));
        assert!(roms_dest.ends_with("game.sh"));
    }

    #[test]
    fn test_deploy_atomic_update_success_zero_copy() {
        std::env::set_var("DEVBRIDGE_MOCK_ROMS", "1");
        let mut mgr = DeployManager::new();
        let payload1 = b"version 1.0 binary data";
        let mut h1 = Sha256::new();
        h1.update(payload1);
        let hash1 = format!("{:x}", h1.finalize());

        // Deploy v1
        mgr.start("ports/my_port", "runner.sh", payload1.len() as u64, &hash1, true)
            .expect("Deploy v1 start");
        mgr.append_chunk(0, payload1).expect("Deploy v1 chunk");
        let (dest1, _) = mgr.finish().expect("Deploy v1 finish");
        let dest_path = PathBuf::from(&dest1);
        assert!(dest_path.exists());
        assert_eq!(fs::read(&dest_path).unwrap(), payload1);

        // Deploy v2 over existing v1 -> deve substituir atomicamente via rename
        let payload2 = b"version 2.0 updated binary data";
        let mut h2 = Sha256::new();
        h2.update(payload2);
        let hash2 = format!("{:x}", h2.finalize());

        mgr.start("ports/my_port", "runner.sh", payload2.len() as u64, &hash2, true)
            .expect("Deploy v2 start");
        mgr.append_chunk(0, payload2).expect("Deploy v2 chunk");
        let (dest2, _) = mgr.finish().expect("Deploy v2 finish");
        assert_eq!(dest1, dest2);
        assert_eq!(fs::read(&dest_path).unwrap(), payload2);

        let _ = fs::remove_file(&dest_path);
    }
}
