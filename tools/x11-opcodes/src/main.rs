use anyhow::{Context, Result};
use serde::Serialize;
use x11rb::protocol::xproto::{ConnectionExt as _, QueryExtensionReply};

#[derive(Serialize)]
struct ExtInfo {
    name: String,
    present: bool,
    major_opcode: u8,
    first_event: u8,
    first_error: u8,
}

#[derive(Serialize)]
struct Report {
    display: Option<String>,
    extensions: Vec<ExtInfo>,
}

fn to_info(name: &str, reply: &QueryExtensionReply) -> ExtInfo {
    ExtInfo {
        name: name.to_string(),
        present: reply.present,
        major_opcode: reply.major_opcode,
        first_event: reply.first_event,
        first_error: reply.first_error,
    }
}

fn main() -> Result<()> {
    let display = std::env::var("DISPLAY").ok();

    let (conn, _screen_num) = x11rb::connect(None).context("connect to X11")?;

    let list = conn
        .list_extensions()
        .context("ListExtensions request")?
        .reply()
        .context("ListExtensions reply")?;

    let mut extensions: Vec<ExtInfo> = Vec::new();

    for name in list.names {
        let name_str = String::from_utf8_lossy(&name.name).to_string();
        let reply = conn
            .query_extension(name_str.as_bytes())
            .with_context(|| format!("QueryExtension request for {name_str}"))?
            .reply()
            .with_context(|| format!("QueryExtension reply for {name_str}"))?;
        extensions.push(to_info(&name_str, &reply));
    }

    extensions.sort_by(|a, b| {
        a.major_opcode
            .cmp(&b.major_opcode)
            .then_with(|| a.name.cmp(&b.name))
    });

    let report = Report {
        display,
        extensions,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
