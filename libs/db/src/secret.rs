//! The password of the world database kept between sessions, as the user chose: in the Credential
//! Manager of Windows (a generic credential of the target `UniWoW world-db <user>@<host>:<port>`,
//! readable by the user's Windows account alone), never in the settings, the project or the log.

use crate::Settings;

/// Where passwords are kept: the Credential Manager of Windows, a vault of memory in the tests.
pub trait Vault: Send + Sync {
    /// The password kept for `target`, none when there is none.
    fn get(&self, target: &str) -> Result<Option<String>, String>;
    fn set(&self, target: &str, user: &str, password: &str) -> Result<(), String>;
    /// Forgets the password of `target`; nothing to forget is no error.
    fn forget(&self, target: &str) -> Result<(), String>;
}

/// The target of the password of the connection `settings`.
pub fn target(settings: &Settings) -> String {
    format!("UniWoW world-db {}@{}:{}", settings.user, settings.host, settings.port)
}

/// The vault of this machine: the Credential Manager of Windows.
#[derive(Default)]
pub struct System;

#[cfg(windows)]
impl Vault for System {
    fn get(&self, target: &str) -> Result<Option<String>, String> {
        windows_vault::get(target)
    }

    fn set(&self, target: &str, user: &str, password: &str) -> Result<(), String> {
        windows_vault::set(target, user, password)
    }

    fn forget(&self, target: &str) -> Result<(), String> {
        windows_vault::forget(target)
    }
}

#[cfg(not(windows))]
impl Vault for System {
    fn get(&self, _target: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn set(&self, _target: &str, _user: &str, _password: &str) -> Result<(), String> {
        Err("a password is kept on Windows alone".to_owned())
    }

    fn forget(&self, _target: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(windows)]
mod windows_vault {
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{HSTRING, PWSTR};

    fn missing(error: &windows::core::Error) -> bool {
        error.code() == ERROR_NOT_FOUND.to_hresult()
    }

    pub fn get(target: &str) -> Result<Option<String>, String> {
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: Windows fills `credential`, freed once its password is copied.
        match unsafe { CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut credential) } {
            Ok(()) => {}
            Err(error) if missing(&error) => return Ok(None),
            Err(error) => return Err(format!("the password kept is not read: {error}")),
        }
        let password = unsafe {
            let read = &*credential;
            let blob = std::slice::from_raw_parts(read.CredentialBlob, read.CredentialBlobSize as usize);
            let units: Vec<u16> = blob
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            let password = String::from_utf16(&units);
            CredFree(credential.cast());
            password
        };
        password
            .map(Some)
            .map_err(|_| "the password kept is not readable".to_owned())
    }

    pub fn set(target: &str, user: &str, password: &str) -> Result<(), String> {
        let mut target: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let mut user: Vec<u16> = user.encode_utf16().chain([0]).collect();
        let mut blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: every buffer lives through the call.
        let written = unsafe { CredWriteW(&credential, 0) };
        blob.fill(0);
        written.map_err(|error| format!("the password is not kept: {error}"))
    }

    pub fn forget(target: &str) -> Result<(), String> {
        // SAFETY: a name given for the call alone.
        match unsafe { CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(error) if missing(&error) => Ok(()),
            Err(error) => Err(format!("the password kept is not forgotten: {error}")),
        }
    }
}

/// A vault of memory, for the tests.
#[derive(Default)]
pub struct Memory(pub std::sync::Mutex<std::collections::HashMap<String, String>>);

impl Vault for Memory {
    fn get(&self, target: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(target).cloned())
    }

    fn set(&self, target: &str, _user: &str, password: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(target.to_owned(), password.to_owned());
        Ok(())
    }

    fn forget(&self, target: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(target);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_names_the_user_the_host_and_the_port() {
        assert_eq!(target(&Settings::default()), "UniWoW world-db acore@127.0.0.1:3306");
    }

    /// Against the Credential Manager of this machine, under a target of the tests alone.
    #[test]
    #[cfg(windows)]
    fn a_password_is_kept_by_windows_read_back_and_forgotten() {
        let target = format!("UniWoW world-db test {}", std::process::id());
        let vault = System;
        let password = "s3cr3t-of-the-test é";
        vault.set(&target, "acore", password).unwrap();
        assert_eq!(vault.get(&target).unwrap().as_deref(), Some(password));
        vault.forget(&target).unwrap();
        assert_eq!(vault.get(&target).unwrap(), None);
        assert!(vault.forget(&target).is_ok(), "nothing to forget is no error");
    }
}
