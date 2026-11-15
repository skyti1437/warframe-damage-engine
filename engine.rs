// SPDX-License-Identifier: AGPL-3.0-or-later
//! 将结构化配装交给冻结版引擎的同步接口。

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const SOURCE: &str = "https://github.com/skyti1437/warframe-damage-engine";

fn dispatch(request: &Value) -> Value {
    match request["path"].as_str() {
        Some("/healthz") => json!({
            "ok": true,
            "engine": "warframe-damage-engine",
            "version": env!("CARGO_PKG_VERSION"),
            "data_revision": include_str!("UPSTREAM").lines().next(),
            "source": SOURCE,
        }),
        Some(path) if request["body"].is_object() => wfsim_webapi::route(path, &request["body"])
            .unwrap_or_else(|| json!({"ok": false, "error": "未知计算接口"})),
        _ => json!({"ok": false, "error": "需要接口路径与 JSON 对象"}),
    }
}

fn main() -> io::Result<()> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => json!({"id": request["id"], "response": dispatch(&request)}),
            Err(_) => json!({"id": null, "response": {"ok": false, "error": "JSON 格式错误"}}),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}
