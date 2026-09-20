import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockEngine, mockLog, mockSetup, mockStartup, mockWindow, tauriEngine, tauriLog, tauriSetup, tauriStartup, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win, log, startup, setup] = inTauri
  ? await Promise.all([tauriEngine(), tauriWindow(), tauriLog(), tauriStartup(), tauriSetup()])
  : [mockEngine(), mockWindow(), mockLog(), mockStartup(), mockSetup()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} log={log} startup={startup} setup={setup} autoStart={!(inTauri && import.meta.env.DEV)} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
