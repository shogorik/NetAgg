import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { inBrowser } from "./api/transport";
import "./styles/global.css";

if (inBrowser) document.documentElement.classList.add("web");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
