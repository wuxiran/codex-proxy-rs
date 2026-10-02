//! 登录票据的静态加密：AES-256-GCM，密钥文件位于 runtime 数据目录，不进数据库与备份。
//!
//! 密文格式：`版本(1) | nonce(12) | ciphertext+tag`；以账号 ID 作为附加认证数据，
//! 密文被挪到别的账号行上无法解开。蓝绿两个槽位共享数据目录，因此共用同一把密钥。

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom as _, SystemRandom},
};

use crate::model::AdminError;

const KEY_FILE: &str = "ticket.key";
const KEY_LEN: usize = 32;
const VERSION: u8 = 1;

pub struct TicketCipher {
    root: PathBuf,
    random: SystemRandom,
}

impl TicketCipher {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            random: SystemRandom::new(),
        }
    }

    /// # Errors
    ///
    /// 密钥不可用或加密失败时返回错误。
    pub fn seal(&self, account_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, AdminError> {
        let key = self.key(true)?;
        let mut nonce = [0_u8; NONCE_LEN];
        self.random
            .fill(&mut nonce)
            .map_err(|_| AdminError::internal("票据加密失败"))?;
        let mut buffer = plaintext.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(account_id.as_bytes()),
            &mut buffer,
        )
        .map_err(|_| AdminError::internal("票据加密失败"))?;
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + buffer.len());
        sealed.push(VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&buffer);
        Ok(sealed)
    }

    /// # Errors
    ///
    /// 密钥缺失、密文属于其他账号或已被篡改时返回错误。
    pub fn open(&self, account_id: &str, sealed: &[u8]) -> Result<Vec<u8>, AdminError> {
        let corrupted =
            || AdminError::internal("票据无法解密（密钥已更换或数据已损坏），请重新录入票据");
        if sealed.len() < 1 + NONCE_LEN || sealed[0] != VERSION {
            return Err(corrupted());
        }
        let key = self.key(false)?;
        let nonce =
            Nonce::try_assume_unique_for_key(&sealed[1..=NONCE_LEN]).map_err(|_| corrupted())?;
        let mut buffer = sealed[1 + NONCE_LEN..].to_vec();
        let plaintext = key
            .open_in_place(nonce, Aad::from(account_id.as_bytes()), &mut buffer)
            .map_err(|_| corrupted())?;
        Ok(plaintext.to_vec())
    }

    fn key(&self, create: bool) -> Result<LessSafeKey, AdminError> {
        let path = self.root.join(KEY_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                create_key(&self.root, &path, &self.random)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdminError::unavailable("票据密钥不存在，请重新录入票据"));
            }
            Err(_) => return Err(AdminError::unavailable("票据密钥暂不可读")),
        };
        if bytes.len() != KEY_LEN {
            return Err(AdminError::internal("票据密钥文件已损坏"));
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes)
            .map_err(|_| AdminError::internal("票据密钥文件已损坏"))?;
        Ok(LessSafeKey::new(key))
    }
}

/// 独占创建密钥文件；两个槽位同时首次写票据时，后到者读取先到者写入的密钥。
fn create_key(root: &Path, path: &Path, random: &SystemRandom) -> Result<Vec<u8>, AdminError> {
    let unavailable = |_| AdminError::unavailable("票据密钥暂不可写");
    fs::create_dir_all(root).map_err(unavailable)?;
    let mut key = vec![0_u8; KEY_LEN];
    random
        .fill(&mut key)
        .map_err(|_| AdminError::internal("票据密钥生成失败"))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&key).map_err(unavailable)?;
            file.sync_all().map_err(unavailable)?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::read(path).map_err(unavailable)
        }
        Err(error) => Err(unavailable(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::TicketCipher;

    #[test]
    fn seals_per_account_and_rejects_moved_or_tampered_ciphertext() {
        let root = std::env::temp_dir().join(format!("cpr-ticket-{}", uuid::Uuid::now_v7()));
        let cipher = TicketCipher::new(root.clone());
        let sealed = cipher.seal("acct_a", b"secret").unwrap();
        assert_eq!(cipher.open("acct_a", &sealed).unwrap(), b"secret");
        assert!(cipher.open("acct_b", &sealed).is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(cipher.open("acct_a", &tampered).is_err());
        // 同一明文两次加密使用不同 nonce。
        assert_ne!(cipher.seal("acct_a", b"secret").unwrap(), sealed);
        std::fs::remove_dir_all(root).unwrap();
    }
}
