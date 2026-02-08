use std::process::Command;
use std::io::Write;
use tempfile::NamedTempFile;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tauri::command;

#[command]
pub async fn extract_text_from_image(base64_image: String) -> Result<String, String> {
    // 1. Decode base64 to bytes
    let image_bytes = BASE64.decode(base64_image)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;

    // 2. Write to temp file
    let mut temp_file = NamedTempFile::new()
        .map_err(|e| format!("Failed to create temp file: {}", e))?;
    
    temp_file.write_all(&image_bytes)
        .map_err(|e| format!("Failed to write image to temp file: {}", e))?;
        
    let file_path = temp_file.path().to_string_lossy().to_string();

    // 3. Run swift script
    // We assume the swift script is located relative to the resource path or we just use `swift` command with the source file
    // Ideally, we should embed the script string here and write it to a temp file too, to avoid path issues.
    
    let swift_script = r#"
import Vision
import Cocoa

guard CommandLine.arguments.count > 1 else { exit(1) }
let imagePath = CommandLine.arguments[1]
let imageUrl = URL(fileURLWithPath: imagePath)
guard let image = NSImage(contentsOf: imageUrl),
      let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { exit(1) }

let request = VNRecognizeTextRequest { (request, error) in
    guard let observations = request.results as? [VNRecognizedTextObservation] else { return }
    let text = observations.compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
    print(text)
}
request.recognitionLevel = .accurate
let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
try? handler.perform([request])
"#;

    let mut script_file = NamedTempFile::new()
        .map_err(|e| format!("Failed to create script file: {}", e))?;
    script_file.write_all(swift_script.as_bytes())
        .map_err(|e| format!("Failed to write script file: {}", e))?;
    let script_path = script_file.path().to_string_lossy().to_string();

    let output = Command::new("swift")
        .arg(&script_path)
        .arg(&file_path)
        .output()
        .map_err(|e| format!("Failed to execute swift command: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("OCR failed: {}", stderr));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(text.trim().to_string())
}


//OKAY BOTHEIN
//SXXXS
//