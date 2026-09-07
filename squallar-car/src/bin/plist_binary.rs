//! Rewrite a property list as a binary plist, in place or to a new path.
//!
//!     cargo run -p squallar-car --bin plist-binary -- <in.plist> [out.plist]
//!
//! Xcode writes every plist it puts in a bundle as `bplist00`; this
//! repository's bundles shipped `Info.plist` as XML. Both are property lists
//! and every Apple reader accepts both, which is why nothing noticed until a
//! validator that only ever sees Xcode's output was asked about ours. The
//! values are untouched: the file is parsed as a plist and written back as a
//! plist, so a key that was there is still there and reads the same.
//!
//! The build container has neither `plutil` nor `python3`, so the conversion
//! lives here, beside the other Apple bundle formats this crate writes.

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, output) = match args.as_slice() {
        [i] => (PathBuf::from(i), PathBuf::from(i)),
        [i, o] => (PathBuf::from(i), PathBuf::from(o)),
        _ => return Err("usage: plist-binary <in.plist> [out.plist]".to_string()),
    };
    let value: plist::Value = plist::from_file(&input)
        .map_err(|e| format!("reading {} as a property list: {e}", input.display()))?;
    let mut bytes = Vec::new();
    plist::to_writer_binary(&mut bytes, &value)
        .map_err(|e| format!("encoding {} as a binary plist: {e}", input.display()))?;
    std::fs::write(&output, &bytes).map_err(|e| format!("writing {}: {e}", output.display()))?;
    println!(
        "  wrote {} ({} bytes, binary plist)",
        output.display(),
        bytes.len()
    );
    Ok(())
}
