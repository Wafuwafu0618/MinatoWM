//! Turns samples into per-label statistics (JSON and a markdown table).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::Sample;

fn stat(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let r = |x: f64| (x * 100.0).round() / 100.0;
    json!({ "p50": r(pct(0.5)), "p95": r(pct(0.95)), "max": r(*v.last().unwrap()), "mean": r(mean), "n": v.len() })
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// Sum of `key` over a sample's batches (or freezes, with `freeze`).
fn per_sample(s: &Sample, get: impl Fn(&Value) -> f64) -> f64 {
    s.batches.iter().map(get).sum()
}

fn freezes(s: &Sample) -> impl Iterator<Item = &Value> {
    s.batches
        .iter()
        .flat_map(|b| b["freezes"].as_array().into_iter().flatten())
}

fn summarize(samples: &[&Sample]) -> Value {
    let col = |g: &dyn Fn(&Sample) -> Option<f64>| stat(samples.iter().filter_map(|s| g(s)).collect());
    let all_freezes: Vec<&Value> = samples.iter().flat_map(|s| freezes(s)).collect();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for fz in &all_freezes {
        *kinds.entry(fz["capture"].as_str().unwrap_or("?").to_string()).or_default() += 1;
    }
    let fresh = all_freezes.iter().filter(|fz| fz["fresh"] == true).count();
    let anims: Vec<&Value> = samples.iter().flat_map(|s| s.anims.iter()).collect();
    let frames: f64 = anims.iter().map(|a| f(&a["frames"])).sum();
    let slow: f64 = anims.iter().map(|a| f(&a["slow_frames"])).sum();
    json!({
        "n": samples.len(),
        "wm_ms": col(&|s| s.wm_ms),
        "settle_ms": col(&|s| Some(s.settle_ms)),
        "anim_start_ms": col(&|s| s.anim_start_ms),
        "anim_end_ms": col(&|s| s.anim_end_ms),
        "batch_total_ms": col(&|s| Some(per_sample(s, |b| f(&b["total_ms"])))),
        "freeze_wait_ms": col(&|s| Some(per_sample(s, |b| f(&b["freeze_wait_ms"])))),
        "freeze": {
            "count": all_freezes.len(),
            "fresh": fresh,
            "capture_kinds": kinds,
            "capture_ms": stat(all_freezes.iter().map(|fz| f(&fz["capture_ms"])).collect()),
            "commit_ms": stat(all_freezes.iter().map(|fz| f(&fz["commit_ms"])).collect()),
            "flush_ms": stat(all_freezes.iter().map(|fz| f(&fz["flush_ms"])).collect()),
            "total_ms": stat(all_freezes.iter().map(|fz| f(&fz["total_ms"])).collect()),
        },
        "set_position_ms": col(&|s| Some(per_sample(s, |b| f(&b["set_position"]["ms"])))),
        "show_ms": col(&|s| Some(per_sample(s, |b| f(&b["show"]["ms"])))),
        "hide_ms": col(&|s| Some(per_sample(s, |b| f(&b["hide"]["ms"])))),
        "focus_ms": col(&|s| Some(per_sample(s, |b| f(&b["focus"]["ms"])))),
        "after_batch_ms": col(&|s| Some(per_sample(s, |b| f(&b["after_batch_ms"])))),
        "anim": {
            "count": anims.len(),
            "frames": frames,
            "slow_frames": slow,
            "max_gap_ms": stat(anims.iter().map(|a| f(&a["max_gap_ms"])).collect()),
            "refresh_ms": anims.first().map(|a| a["refresh_ms"].clone()),
        },
    })
}

pub fn document(runs: &[(String, Vec<Sample>)]) -> Value {
    let scenarios: Vec<Value> = runs
        .iter()
        .map(|(name, samples)| {
            let mut labels: Vec<String> = Vec::new();
            for s in samples {
                if !labels.contains(&s.label) {
                    labels.push(s.label.clone());
                }
            }
            let summary: Vec<Value> = labels
                .iter()
                .map(|l| {
                    let group: Vec<&Sample> = samples.iter().filter(|s| &s.label == l).collect();
                    json!({ "label": l, "stats": summarize(&group) })
                })
                .collect();
            json!({ "name": name, "summary": summary, "samples": samples })
        })
        .collect();
    json!({ "scenarios": scenarios })
}

pub fn markdown(doc: &Value) -> String {
    let p = |v: &Value| match (v["p50"].as_f64(), v["p95"].as_f64()) {
        (Some(a), Some(b)) => format!("{a:.1} / {b:.1}"),
        _ => "-".into(),
    };
    let mut out = String::new();
    for sc in doc["scenarios"].as_array().into_iter().flatten() {
        out.push_str(&format!("\n### {}\n\n", sc["name"].as_str().unwrap_or("?")));
        out.push_str("| label | n | wm | settle | anim start | anim end | freeze wait | capture | flush | fresh | set_pos | show | hide | focus | slow frames |\n");
        out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
        for row in sc["summary"].as_array().into_iter().flatten() {
            let s = &row["stats"];
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {} | {} | {} | {} | {}/{} |\n",
                row["label"].as_str().unwrap_or("?"),
                s["n"],
                p(&s["wm_ms"]),
                p(&s["settle_ms"]),
                p(&s["anim_start_ms"]),
                p(&s["anim_end_ms"]),
                p(&s["freeze_wait_ms"]),
                p(&s["freeze"]["capture_ms"]),
                p(&s["freeze"]["flush_ms"]),
                s["freeze"]["fresh"],
                s["freeze"]["count"],
                p(&s["set_position_ms"]),
                p(&s["show_ms"]),
                p(&s["hide_ms"]),
                p(&s["focus_ms"]),
                s["anim"]["slow_frames"],
                s["anim"]["frames"],
            ));
        }
    }
    out.push_str("\n(ms, p50 / p95)\n");
    out
}
