// The hidden main window's entry point.
//
// Everything the user sees is in src/hud/ and src/settings/, which are separate Vite entry
// points with their own windows. The main window is never shown — it exists because Tauri wants
// one, and this file exists so Vite has something to build for it.
export {};
