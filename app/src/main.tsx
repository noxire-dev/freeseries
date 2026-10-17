import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import App from "./App";
import { githubAutoEq, mockAgent, mockHeadset, mockApps, mockEngine, mockHotkeys, mockInfo, mockLog, mockMic, mockSetup, mockStartup, mockTray, mockWindow, tauriAgent, tauriHeadset, tauriApps, tauriEngine, tauriHotkeys, tauriInfo, tauriLog, tauriMic, tauriSetup, tauriStartup, tauriTray, tauriWindow } from "./api";
import { localSettings } from "./store";

const inTauri = "__TAURI_INTERNALS__" in window;
const [engine, win, log, startup, setup, info, hotkeys, apps, agent, headset, mic, tray] = inTauri
  ? await Promise.all([tauriEngine(), tauriWindow(), tauriLog(), tauriStartup(), tauriSetup(), tauriInfo(), tauriHotkeys(), tauriApps(), tauriAgent(), tauriHeadset(), tauriMic(), tauriTray()])
  : [mockEngine(), mockWindow(), mockLog(), mockStartup(), mockSetup(), mockInfo(), mockHotkeys(), mockApps(), mockAgent(), mockHeadset(), mockMic(), mockTray()];

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App engine={engine} win={win} log={log} startup={startup} setup={setup} info={info} hotkeys={hotkeys} apps={apps} agent={agent} headset={headset} mic={mic} tray={tray} autoStart={!(inTauri && import.meta.env.DEV)} autoEq={githubAutoEq()} settings={localSettings()} />
  </React.StrictMode>,
);
