import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { inBrowser } from "./transport";

/** Keeps the popup window exactly as tall as its content, anchored above the tray. */
export function useFitWindow<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el || inBrowser) return;
    let last = 0;
    const observer = new ResizeObserver(() => {
      const height = Math.ceil(el.getBoundingClientRect().height);
      if (height !== last) {
        last = height;
        void invoke("fit_popup", { height });
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return ref;
}

/** Hides the popup back into the tray; a browser page has no tray. */
export const hidePopup = inBrowser ? undefined : () => invoke("hide_popup");
