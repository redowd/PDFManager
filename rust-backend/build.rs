use std::path::{Path, PathBuf};
use std::process::Command;

/// 查找 Windows SDK 里的 rc.exe（优先环境变量 RC，其次取最高版本的 x64 工具）
fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(kits) {
        for entry in entries.flatten() {
            let rc = entry.path().join("x64").join("rc.exe");
            if !rc.is_file() {
                continue;
            }
            let version: Vec<u32> = entry
                .file_name()
                .to_string_lossy()
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();
            if best.as_ref().map(|(v, _)| version > *v).unwrap_or(true) {
                best = Some((version, rc));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Some(rc_exe) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe，跳过 exe 图标与版本资源");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR 缺失"));
    let ico = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("app.ico");
    // rc.exe 会把字符串里的反斜杠当转义（\b、\r 等），统一改用正斜杠
    let ico_rc = ico.display().to_string().replace('\\', "/");

    // 把 Cargo 版本（SemVer）转成 Windows 版本资源的四段数字：1.2.3 -> 1,2,3,0
    let pkg_version = env!("CARGO_PKG_VERSION");
    let mut nums: Vec<u32> = pkg_version
        .split(|c: char| c == '.' || c == '-' || c == '+')
        .filter_map(|s| s.parse().ok())
        .collect();
    nums.truncate(4);
    while nums.len() < 4 {
        nums.push(0);
    }
    let file_version = format!("{},{},{},{}", nums[0], nums[1], nums[2], nums[3]);

    let rc_text = format!(
        "1 ICON \"{ico}\"\n\
         1 VERSIONINFO\n\
         FILEVERSION {ver}\n\
         PRODUCTVERSION {ver}\n\
         FILEFLAGSMASK 0x3fL\n\
         FILEFLAGS 0x0L\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         FILESUBTYPE 0x0L\n\
         BEGIN\n\
           BLOCK \"StringFileInfo\"\n\
           BEGIN\n\
             BLOCK \"040904b0\"\n\
             BEGIN\n\
               VALUE \"FileDescription\", \"PDF Manager\"\n\
               VALUE \"FileVersion\", \"{pkg}\"\n\
               VALUE \"InternalName\", \"pdf-manager\"\n\
               VALUE \"OriginalFilename\", \"PDFManager.exe\"\n\
               VALUE \"ProductName\", \"PDF Manager\"\n\
               VALUE \"ProductVersion\", \"{pkg}\"\n\
             END\n\
           END\n\
           BLOCK \"VarFileInfo\"\n\
           BEGIN\n\
             VALUE \"Translation\", 0x409, 1200\n\
           END\n\
         END\n",
        ico = ico_rc,
        ver = file_version,
        pkg = pkg_version
    );
    let rc_src = out.join("app.rc");
    if std::fs::write(&rc_src, rc_text).is_err() {
        println!("cargo:warning=写入资源脚本失败，跳过图标与版本资源");
        return;
    }
    let res = out.join("app.res");
    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg(format!("/fo{}", res.display()))
        .arg(&rc_src)
        .output();
    match status {
        Ok(o) if o.status.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        Ok(o) => println!(
            "cargo:warning=rc.exe 编译资源失败：{}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => println!("cargo:warning=无法执行 rc.exe：{}", e),
    }
}
