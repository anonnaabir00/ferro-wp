//! macOS Keychain storage for generated site admin passwords.
//! Service "ferro", account = site name.
use std::io;
use std::process::Command;

const SERVICE: &str = "ferro";

pub fn set_password(account: &str, password: &str) -> io::Result<()> {
    let status = Command::new("security")
        .args(["add-generic-password", "-s", SERVICE, "-a", account, "-w"])
        .arg(password)
        .arg("-U")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "security add-generic-password failed — is the login keychain unlocked?",
        ))
    }
}

pub fn get_password(account: &str) -> io::Result<Option<String>> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let password = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(Some(password))
}

pub fn delete_password(account: &str) {
    let _ = Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", account])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
