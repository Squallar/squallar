//! Write a platform's icon set from `packaging/icon/squallar.svg`.
//!
//! Called by the packaging steps, one flag per target:
//!
//!     cargo run -p squallar-icon -- --web       squallar-web/icons
//!     cargo run -p squallar-icon -- --ios       packaging/ios/AppIcon.appiconset
//!     cargo run -p squallar-icon -- --icns      packaging/macos/build/squallar.icns
//!     cargo run -p squallar-icon -- --ico       packaging/windows/squallar.ico
//!     cargo run -p squallar-icon -- --android   packaging/android/app/src/main/res
//!     cargo run -p squallar-icon -- --hicolor   packaging/linux/hicolor
//!
//! Nothing it writes is committed. Every output directory is gitignored, and
//! the build regenerates what it needs, so there is exactly one drawing of this
//! icon in the repository and it is the SVG.

use std::path::{Path, PathBuf};

use squallar_icon::{ICNS_MEMBERS, ICO_SIZES, Icon, icns, ico};

/// The web set. Names match what `squallar-web/index.html` links and what
/// `sw.js` precaches; `squallar-web/tests/pwa_assets.rs` asserts the two agree
/// with this list, so a rename here that is not made there is a red test rather
/// than a 404 on a deployed page.
const WEB: [(&str, u32); 5] = [
    ("favicon-32.png", 32),
    ("icon-192.png", 192),
    ("icon-512.png", 512),
    ("icon-maskable-512.png", 512),
    ("apple-touch-icon.png", 180),
];

/// Android launcher densities: the legacy square icon.
const ANDROID_LEGACY: [(&str, u32); 5] = [
    ("mdpi", 48),
    ("hdpi", 72),
    ("xhdpi", 96),
    ("xxhdpi", 144),
    ("xxxhdpi", 192),
];

/// Adaptive layers are 108dp with only the middle 72dp guaranteed visible,
/// because every launcher masks them to a different shape.
const ANDROID_ADAPTIVE: [(&str, u32); 5] = [
    ("mdpi", 108),
    ("hdpi", 162),
    ("xhdpi", 216),
    ("xxhdpi", 324),
    ("xxxhdpi", 432),
];
const ANDROID_SAFE_FRACTION: f32 = 72.0 / 108.0;

/// The freedesktop icon theme sizes. A `.desktop` entry names the icon and the
/// theme resolves it per size, so shipping one big PNG makes every small
/// rendering a downscale done by whichever launcher happens to be running.
const HICOLOR: [u32; 7] = [16, 22, 24, 32, 48, 128, 256];

/// The loose iOS icon set, written into the bundle ROOT.
///
/// Not an asset catalog, and that is forced rather than chosen: a catalog is a
/// compiled `Assets.car`, the only compiler for it is Xcode's `actool`, and
/// this build has no Mac in it. Loose files named by the convention below are
/// what an app can ship without one, and they are what satisfies the two size
/// errors (90022 wants 120x120, 90023 wants 152x152).
///
/// `(file name without .png, pixels)`.
const IOS_LOOSE: [(&str, u32); 13] = [
    ("AppIcon20x20@2x", 40),
    ("AppIcon20x20@3x", 60),
    ("AppIcon29x29@2x", 58),
    ("AppIcon29x29@3x", 87),
    ("AppIcon40x40@2x", 80),
    ("AppIcon40x40@3x", 120),
    ("AppIcon60x60@2x", 120),
    ("AppIcon60x60@3x", 180),
    ("AppIcon20x20~ipad", 20),
    ("AppIcon29x29~ipad", 29),
    ("AppIcon40x40~ipad", 40),
    ("AppIcon76x76@2x~ipad", 152),
    ("AppIcon83.5x83.5@2x~ipad", 167),
];

const IOS_CONTENTS: &str = r#"{
  "images" : [
    {
      "filename" : "AppIcon.png",
      "idiom" : "universal",
      "platform" : "ios",
      "size" : "1024x1024"
    }
  ],
  "info" : {
    "author" : "squallar",
    "version" : 1
  }
}
"#;

const ANDROID_ADAPTIVE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background"/>
    <foreground android:drawable="@mipmap/ic_launcher_foreground"/>
    <monochrome android:drawable="@mipmap/ic_launcher_foreground"/>
</adaptive-icon>
"#;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Err(usage());
    }

    let svg_path = repo_root().join("packaging/icon/squallar.svg");
    let svg =
        std::fs::read(&svg_path).map_err(|e| format!("reading {}: {e}", svg_path.display()))?;
    let icon = Icon::parse(&svg)?;

    let mut i = 0;
    while i < args.len() {
        let (flag, dest) = match (args.get(i), args.get(i + 1)) {
            (Some(f), Some(d)) => (f.as_str(), PathBuf::from(d)),
            _ => return Err(usage()),
        };
        match flag {
            "--web" => web(&icon, &dest)?,
            "--ios" => ios(&icon, &dest)?,
            "--icns" => macos(&icon, &dest)?,
            "--ico" => windows(&icon, &dest)?,
            "--android" => android(&icon, &dest)?,
            "--hicolor" => hicolor(&icon, &dest)?,
            other => return Err(format!("unknown target {other:?}\n\n{}", usage())),
        }
        i += 2;
    }
    Ok(())
}

fn usage() -> String {
    "usage: squallar-icon (--web|--ios|--icns|--ico|--android|--hicolor) PATH ...\n\
     \n\
     Writes one platform's icons from packaging/icon/squallar.svg. Several\n\
     targets may be given in one run. Nothing written here is committed."
        .to_string()
}

/// The workspace root, found from this crate's manifest directory rather than
/// from the current directory, so the caller can invoke it from anywhere.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate directory has a parent")
        .to_path_buf()
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!("  wrote {}", path.display());
    Ok(())
}

fn web(icon: &Icon, dir: &Path) -> Result<(), String> {
    for (name, px) in WEB {
        write(&dir.join(name), &icon.png(px))?;
    }
    Ok(())
}

fn ios(icon: &Icon, bundle: &Path) -> Result<(), String> {
    // Loose PNGs at the bundle root, plus the source catalog beside them.
    //
    // The catalog is written but NOT compiled: `actool` is Xcode-only, so
    // there is no `Assets.car` here and the loose files are what the bundle
    // actually ships. The `.appiconset` is kept because it is the input any
    // future compile would take, and because it is where the 1024 marketing
    // icon belongs.
    for (name, px) in IOS_LOOSE {
        write(&bundle.join(format!("{name}.png")), &icon.png(px))?;
    }
    let catalog = bundle.join("AppIcon.appiconset");
    write(&catalog.join("AppIcon.png"), &icon.png(1024))?;
    write(&catalog.join("Contents.json"), IOS_CONTENTS.as_bytes())
}

fn macos(icon: &Icon, path: &Path) -> Result<(), String> {
    let members: Vec<(&[u8; 4], Vec<u8>)> = ICNS_MEMBERS
        .iter()
        .map(|(px, ostype)| (*ostype, icon.png(*px)))
        .collect();
    write(path, &icns(&members))
}

fn windows(icon: &Icon, path: &Path) -> Result<(), String> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|&px| (px, icon.png(px))).collect();
    write(path, &ico(&images))
}

fn android(icon: &Icon, res: &Path) -> Result<(), String> {
    for (density, px) in ANDROID_LEGACY {
        let dir = res.join(format!("mipmap-{density}"));
        let png = icon.png(px);
        write(&dir.join("ic_launcher.png"), &png)?;
        write(&dir.join("ic_launcher_round.png"), &png)?;
    }
    for (density, px) in ANDROID_ADAPTIVE {
        let png = icon
            .inset(px, ANDROID_SAFE_FRACTION)
            .encode_png()
            .map_err(|e| format!("encoding the adaptive foreground: {e}"))?;
        write(
            &res.join(format!("mipmap-{density}"))
                .join("ic_launcher_foreground.png"),
            &png,
        )?;
    }
    let anydpi = res.join("mipmap-anydpi-v26");
    write(
        &anydpi.join("ic_launcher.xml"),
        ANDROID_ADAPTIVE_XML.as_bytes(),
    )?;
    write(
        &anydpi.join("ic_launcher_round.xml"),
        ANDROID_ADAPTIVE_XML.as_bytes(),
    )?;

    // The adaptive background is a flat fill of the drawing's own ground,
    // sampled from a render rather than typed here, so it cannot differ from
    // what the SVG paints.
    let ground = icon.render(2).pixel(0, 0).expect("2px render has pixels");
    let colour = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <resources>\n    \
         <color name=\"ic_launcher_background\">#{:02x}{:02x}{:02x}</color>\n\
         </resources>\n",
        ground.red(),
        ground.green(),
        ground.blue()
    );
    write(
        &res.join("values").join("ic_launcher_background.xml"),
        colour.as_bytes(),
    )
}

fn hicolor(icon: &Icon, dir: &Path) -> Result<(), String> {
    for px in HICOLOR {
        write(
            &dir.join(format!("{px}x{px}"))
                .join("apps")
                .join("app.squallar.png"),
            &icon.png(px),
        )?;
    }
    Ok(())
}
