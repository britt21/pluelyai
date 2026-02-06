import Vision
import Cocoa

// Disable stderr to keep output clean? Or just handle stdout.
// Simple script to extract text from an image file path provided as argument.

guard CommandLine.arguments.count > 1 else {
    print("Error: No image path provided")
    exit(1)
}

let imagePath = CommandLine.arguments[1]
let imageUrl = URL(fileURLWithPath: imagePath)

guard let image = NSImage(contentsOf: imageUrl),
      let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    print("Error: Could not load image")
    exit(1)
}

let request = VNRecognizeTextRequest { (request, error) in
    if let error = error {
        print("Error: \(error)")
        exit(1)
    }
    guard let observations = request.results as? [VNRecognizedTextObservation] else { 
        print("") 
        return 
    }
    let text = observations.compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
    print(text)
}

// Configure for accuracy
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do {
    try handler.perform([request])
} catch {
    print("Error: \(error)")
    exit(1)
}
