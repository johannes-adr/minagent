//! The `view_image` tool (feature `image`): shows an image file to the model.

use crate::harness::{Approval, Context, ToolPolicy};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rig_agent::rig_tool;
use rig_core::message::{ImageMediaType, ToolResultContent};
use rig_core::tool::{ToolExecutionError, ToolOutput};
use std::path::Path;

/// Largest image file the tool sends (the per-image limit of the strictest provider).
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Show an image file to yourself: it is returned as an image you can look at. Use it for
/// screenshots, charts, diagrams and photos, e.g. after creating one with `execute`.
/// Only PNG, JPEG, GIF and WebP files up to 5 MiB work (the type is read from the file's
/// content, not its name); anything else is rejected. The file's content is sent to the
/// model provider.
#[rig_tool(params(
    path = "Path of the image file, relative to the `work` directory or absolute."
))]
pub(crate) async fn view_image(path: String) -> Result<ToolOutput, ToolExecutionError> {
    load(&path).await
}

/// Reads the file at `path` (relative to the work directory, or absolute) and returns it
/// as an image result.
async fn load(path: &str) -> Result<ToolOutput, ToolExecutionError> {
    let fail = |e: std::io::Error| ToolExecutionError::other(format!("cannot read {path}: {e}"));
    let file = crate::tools::workdir().join(path);
    let size = tokio::fs::metadata(&file).await.map_err(fail)?.len();
    if size > MAX_IMAGE_BYTES {
        return Err(ToolExecutionError::other(format!(
            "{path} is {size} bytes; the limit is {MAX_IMAGE_BYTES}. Downscale it first, e.g. with `execute`."
        )));
    }
    let bytes = tokio::fs::read(&file).await.map_err(fail)?;
    image_output(path, &bytes)
}

/// The file is sent to the model provider. Images inside the work directory (or any
/// image in a container) are shown without asking; others need the user's approval.
impl ToolPolicy for ViewImage {
    const KIND: &'static str = "read";

    fn prompt(_context: &Context) -> String {
        "Use the `view_image` tool to look at an image file (PNG, JPEG, GIF, WebP); \
         `execute` and `cat` cannot show images. Relative paths are relative to the `work` \
         directory, so save images in `work/` (e.g. with `execute`). The model must support \
         image input."
            .to_owned()
    }

    fn approval(args: &ViewImageParameters, context: &Context) -> Approval {
        approval_in(&args.path, context, &crate::tools::workdir())
    }
}

fn approval_in(path: &str, context: &Context, workdir: &Path) -> Approval {
    if context.isolated {
        Approval::auto(format!("[isolated] view_image: {path}"))
    } else if inside(path, workdir) {
        Approval::auto(format!("view_image: {path}"))
    } else {
        Approval::ask(format!("view_image outside the work directory: {path}"))
    }
}

/// Whether `path` (relative to `workdir` or absolute) leads to a file inside `workdir`
/// once `..` and symlinks are resolved. A path that cannot be resolved is not.
fn inside(path: &str, workdir: &Path) -> bool {
    let (Ok(file), Ok(root)) = (workdir.join(path).canonicalize(), workdir.canonicalize()) else {
        return false;
    };
    file.starts_with(root)
}

/// `bytes` as an image result, or an error if they are not a supported image.
fn image_output(path: &str, bytes: &[u8]) -> Result<ToolOutput, ToolExecutionError> {
    let Some(media_type) = ImageMediaType::sniff(bytes) else {
        return Err(ToolExecutionError::other(format!(
            "{path} is not a PNG, JPEG, GIF or WebP image"
        )));
    };
    ToolOutput::content(vec![
        ToolResultContent::text(format!(
            "{path} ({}, {} bytes)",
            media_type.extension(),
            bytes.len()
        )),
        ToolResultContent::image_base64(STANDARD.encode(bytes), Some(media_type), None),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::DocumentSourceKind;

    const PNG_1X1: &[u8] = b"\x89PNG\r\n\x1a\n rest of the file";

    #[test]
    fn an_image_becomes_text_plus_base64_image() {
        let out = image_output("a.png", PNG_1X1).unwrap();
        let [
            ToolResultContent::Text(text),
            ToolResultContent::Image(image),
        ] = out.as_content()
        else {
            panic!("{out:?}");
        };
        assert_eq!(text.text, format!("a.png (png, {} bytes)", PNG_1X1.len()));
        assert_eq!(image.media_type, Some(ImageMediaType::PNG));
        assert_eq!(
            image.data,
            DocumentSourceKind::Base64(STANDARD.encode(PNG_1X1))
        );
    }

    #[test]
    fn other_files_are_rejected_by_content_not_name() {
        for bytes in [&b"SECRET=1\n"[..], b""] {
            let err = image_output("photo.png", bytes).unwrap_err();
            assert!(err.message().contains("not a PNG"), "{}", err.message());
        }
        assert!(image_output("x.txt", PNG_1X1).is_ok());
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agent_view_approval_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        dir
    }

    /// Only images that resolve into the working directory skip the question.
    #[test]
    fn only_images_outside_the_workdir_need_approval() {
        let dir = temp("a");
        let work = dir.join("work");
        std::fs::write(work.join("in.png"), PNG_1X1).unwrap();
        std::fs::write(dir.join("out.png"), PNG_1X1).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("out.png"), work.join("link.png")).unwrap();
        let host = Context { isolated: false };
        let ask = |p: &str| approval_in(p, &host, &work).ask;
        assert!(!ask("in.png"));
        assert!(!ask(work.join("in.png").to_str().unwrap()));
        assert!(ask("../out.png"));
        assert!(ask(dir.join("out.png").to_str().unwrap()));
        #[cfg(unix)]
        assert!(ask("link.png"));
        assert!(ask("missing.png"));
        let in_container = approval_in("../out.png", &Context { isolated: true }, &work);
        assert_eq!(
            in_container,
            Approval::auto("[isolated] view_image: ../out.png")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn load_owned(path: String) -> Result<ToolOutput, ToolExecutionError> {
        load(&path).await
    }

    #[tokio::test]
    async fn missing_and_oversized_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("agent_view_image_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.png");
        std::fs::write(&big, vec![0u8; MAX_IMAGE_BYTES as usize + 1]).unwrap();
        let small = dir.join("small.png");
        std::fs::write(&small, PNG_1X1).unwrap();

        let call = |p: &std::path::Path| load_owned(p.to_string_lossy().into_owned());
        let missing = call(&dir.join("nope.png")).await.unwrap_err();
        assert!(
            missing.message().contains("cannot read"),
            "{}",
            missing.message()
        );
        let too_big = call(&big).await.unwrap_err();
        assert!(too_big.message().contains("limit"), "{}", too_big.message());
        assert!(call(&small).await.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
