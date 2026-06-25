import { useEffect, useState } from "react";

/** Phone-sized viewport (≤ 768px). Reactive — re-renders on resize/rotate. Drives the single-pane
 * mobile shell vs the desktop two-pane layout. */
export function useIsMobile(): boolean {
  const query = "(max-width: 768px)";
  const [mobile, setMobile] = useState(
    () => typeof window !== "undefined" && window.matchMedia(query).matches,
  );
  useEffect(() => {
    const mq = window.matchMedia(query);
    const on = () => setMobile(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return mobile;
}
