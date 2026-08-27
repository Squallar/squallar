//! The one-shot CARTO-to-squallar style conversion.
//!
//! Ran once, on 2026-08-27, to seed `www/styles/`. Those files are owned source
//! from that moment on and are edited directly; nothing regenerates them and
//! nothing compares them against upstream. This binary stays in the tree so the
//! seed is reproducible and reviewable, not so it can be run again on a
//! schedule.
//!
//! ```text
//! basemap-style --theme dark \
//!               --name "Squallar Dark" \
//!               --input  dark-matter.json \
//!               --output www/styles/dark.json
//! ```
//!
//! The input is a local file, never a URL. This tool has no HTTP stack and is
//! not going to grow one: the fetch is a single documented `curl` of a style
//! *document* from CARTO's public repository, recorded in DECISIONS.md with its
//! upstream commit and SHA-256. No tile is ever fetched.

use basemap_style::{Expectation, convert};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    theme: String,
    name: String,
    input: PathBuf,
    output: PathBuf,
    upstream_url: String,
    upstream_commit: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("basemap-style: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let source = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("reading {}: {e}", args.input.display()))?;
    let input: Value = serde_json::from_str(&source)
        .map_err(|e| format!("parsing {}: {e}", args.input.display()))?;

    let mut provenance = Map::new();
    provenance.insert("squallar:upstream".into(), json!(args.upstream_url));
    provenance.insert(
        "squallar:upstream-commit".into(),
        json!(args.upstream_commit),
    );
    provenance.insert("squallar:theme".into(), json!(args.theme));
    provenance.insert(
        "squallar:note".into(),
        json!(
            "Converted once from a CARTO style DOCUMENT (BSD-3-Clause code, CC-BY-4.0 design); no \
             CARTO tile was fetched and none is redistributed. Owned source from 2026-08-27 -- \
             edit this file directly. See tools/basemap-style/DECISIONS.md."
        ),
    );

    let (style, report) = convert(&input, &args.name, &provenance).map_err(|e| e.to_string())?;

    // Check the output before writing it, with the same checker the test suite
    // runs. A converter that only reports on itself is not evidence.
    let expectation = Expectation {
        input_layers: report.input_layers,
        deliberate_drops: basemap_style::DELIBERATE_DROPS
            .iter()
            .map(|(sl, why)| ((*sl).to_owned(), (*why).to_owned()))
            .collect(),
    };
    let findings = basemap_style::check(&style, &expectation);
    if !findings.is_empty() {
        for finding in &findings {
            eprintln!("basemap-style: FINDING: {finding}");
        }
        return Err(format!(
            "{} finding(s); refusing to write {}",
            findings.len(),
            args.output.display()
        ));
    }

    let mut rendered = serde_json::to_string_pretty(&style)
        .map_err(|e| format!("serialising the converted style: {e}"))?;
    rendered.push('\n');
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&args.output, &rendered)
        .map_err(|e| format!("writing {}: {e}", args.output.display()))?;

    println!("{} -> {}", args.input.display(), args.output.display());
    println!(
        "  layers            {} -> {}",
        report.input_layers, report.output_layers
    );
    println!(
        "  phases            {} ground, {} label",
        report.ground_layers, report.label_layers
    );
    println!("  dropped           {}", report.dropped.len());
    for (id, source_layer, reason) in &report.dropped {
        println!("    `{id}` (source-layer `{source_layer}`): {reason}");
    }
    println!("  source-layer renames   {}", report.renamed.len());
    for (id, from, to) in &report.renamed {
        println!("    `{id}`: {from} -> {to}");
    }
    println!(
        "  stop sets         {} -> expressions, {} collapsed to scalars",
        report.stop_sets_to_expressions, report.stop_sets_collapsed_to_scalars
    );
    println!("  text-fields       {}", report.text_fields_rewritten);
    println!("  legacy filters    {}", report.not_in_filters_rewritten);
    println!("  zoom ranges folded     {}", report.zoom_ranges_folded);
    println!("  checker findings  0");

    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut theme = None;
    let mut name = None;
    let mut input = None;
    let mut output = None;
    let mut upstream_url = None;
    let mut upstream_commit = None;

    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut take = |what: &str| argv.next().ok_or_else(|| format!("{what} expects a value"));
        match flag.as_str() {
            "--theme" => theme = Some(take("--theme")?),
            "--name" => name = Some(take("--name")?),
            "--input" => input = Some(PathBuf::from(take("--input")?)),
            "--output" => output = Some(PathBuf::from(take("--output")?)),
            "--upstream-url" => upstream_url = Some(take("--upstream-url")?),
            "--upstream-commit" => upstream_commit = Some(take("--upstream-commit")?),
            "--help" | "-h" => {
                println!(
                    "usage: basemap-style --theme <dark|light> --name <display name> \\\n         \
                     --input <carto style.json> --output <path> \\\n         \
                     [--upstream-url <url>] [--upstream-commit <sha>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag `{other}`")),
        }
    }

    Ok(Args {
        theme: theme.ok_or("--theme is required")?,
        name: name.ok_or("--name is required")?,
        input: input.ok_or("--input is required")?,
        output: output.ok_or("--output is required")?,
        upstream_url: upstream_url.unwrap_or_else(|| "<unrecorded>".to_owned()),
        upstream_commit: upstream_commit.unwrap_or_else(|| "<unrecorded>".to_owned()),
    })
}
