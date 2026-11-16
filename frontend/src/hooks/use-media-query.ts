import * as React from "react";

/** Tracks a CSS media query; `false` where there is no window to ask. */
export function useMediaQuery(query: string) {
  return React.useSyncExternalStore(
    (onChange) => {
      const media = window.matchMedia(query);
      media.addEventListener("change", onChange);
      return () => media.removeEventListener("change", onChange);
    },
    () => window.matchMedia(query).matches,
    () => false,
  );
}
