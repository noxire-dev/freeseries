import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockEngine, mockWindow, tauriEngine, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win] = inTauri ? await Promise.all([tauriEngine(), tauriWindow()]) : [mockEngine(), mockWindow()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
