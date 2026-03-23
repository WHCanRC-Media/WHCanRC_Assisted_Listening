use tracing::info;

/// Generate a systemd unit file for the service.
pub fn generate_systemd_unit() -> String {
    r#"[Unit]
Description=WHCanRC Assisted Listening Server
After=network.target sound.target
Wants=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/whcanrc-assisted-listening
WorkingDirectory=/etc/whcanrc
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=whcanrc

[Install]
WantedBy=multi-user.target
"#
    .to_string()
}

/// Install the systemd unit file to the system.
pub fn install_systemd_unit() -> anyhow::Result<()> {
    let unit = generate_systemd_unit();
    let path = "/etc/systemd/system/whcanrc-assisted-listening.service";
    std::fs::write(path, unit)?;
    info!("Installed systemd unit to {}", path);
    info!("Run: sudo systemctl daemon-reload && sudo systemctl enable --now whcanrc-assisted-listening");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_systemd_unit_contains_required_fields() {
        let unit = generate_systemd_unit();
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("ExecStart=/usr/local/bin/whcanrc-assisted-listening"));
        assert!(unit.contains("Restart=on-failure"));
    }
}
