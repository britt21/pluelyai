import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Overlay from "./components/Overlay";
import { AppProvider, ThemeProvider } from "./contexts";
import "./global.css";

let windowLabel = "main";

try {
  // Only available inside Tauri
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const currentWindow = getCurrentWindow();
  windowLabel = currentWindow.label;
} catch (e) {
  console.warn("Tauri APIs not available – running in browser mode");
}

// Render components
if (windowLabel === "capture-overlay") {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <Overlay />
    </React.StrictMode>
  );
} else {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <ThemeProvider>
        <AppProvider>
          <App />
        </AppProvider>
      </ThemeProvider>
    </React.StrictMode>
  );
}
