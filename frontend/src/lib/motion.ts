/**
 * "Ink & Signal" shared motion — restrained, silky, reduced-motion aware.
 *
 * Durations stay short (120–260ms) and eases are tasteful. The global CSS already
 * neutralizes CSS transitions/animations under `prefers-reduced-motion`; this module
 * covers JS-driven (framer-motion) animation. Use `useMotionOK()` to no-op motion when
 * the user prefers reduced motion.
 */
import {
  useReducedMotion,
  type Transition,
  type Variants,
} from "framer-motion";

/** Default tasteful ease (a soft, slightly-overshootless out-curve). */
export const ease = [0.22, 1, 0.36, 1] as const;

/** Default spring for interactive elements — quick, low-bounce. */
export const spring: Transition = {
  type: "spring",
  stiffness: 420,
  damping: 34,
  mass: 0.9,
};

/** Standard timed transition for entrances. */
export const transition: Transition = { duration: 0.22, ease };

/** A short, snappy transition for hover/tap micro-interactions. */
export const transitionFast: Transition = { duration: 0.13, ease };

/** Fade + small upward slide — the workhorse entrance for content. */
export const fadeSlideUp: Variants = {
  hidden: { opacity: 0, y: 8 },
  visible: { opacity: 1, y: 0, transition },
  exit: { opacity: 0, y: 8, transition: transitionFast },
};

/** Plain fade — for overlays / scrims. */
export const fade: Variants = {
  hidden: { opacity: 0 },
  visible: { opacity: 1, transition: transitionFast },
  exit: { opacity: 0, transition: transitionFast },
};

/** Stagger container for lists — pair with `fadeSlideUp` on each child. */
export const listStagger: Variants = {
  hidden: {},
  visible: { transition: { staggerChildren: 0.04, delayChildren: 0.02 } },
};

/** Side-sheet / panel slide-in from the edge. */
export const sheet: Variants = {
  hidden: { opacity: 0, x: 16 },
  visible: { opacity: 1, x: 0, transition },
  exit: { opacity: 0, x: 16, transition: transitionFast },
};

/** Dialog / popover scale-fade entrance. */
export const popIn: Variants = {
  hidden: { opacity: 0, scale: 0.96, y: 4 },
  visible: { opacity: 1, scale: 1, y: 0, transition },
  exit: { opacity: 0, scale: 0.96, y: 4, transition: transitionFast },
};

/**
 * Returns `true` when JS-driven motion should run, `false` when the user prefers
 * reduced motion. Components should gate animation props on this:
 *
 *   const ok = useMotionOK();
 *   <motion.div initial={ok ? "hidden" : false} animate="visible" variants={fadeSlideUp} />
 */
export function useMotionOK(): boolean {
  return !useReducedMotion();
}
