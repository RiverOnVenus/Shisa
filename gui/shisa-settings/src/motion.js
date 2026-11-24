// One motion language for both windows.
//
// The CSS tokens in app.css carry the same curves for declarative transitions.
// These helpers cover the two things CSS cannot do well here: transitions that
// Svelte starts from script, and interruptions. A fold that is clicked again
// mid-animation must reverse from its current height, not jump, and every
// transition must collapse to an instant change when the desktop asks for
// reduced motion.

const REDUCED = "(prefers-reduced-motion: reduce)";

export const EASE_OUT = "cubic-bezier(0.16, 1, 0.3, 1)";
export const EASE_IN = "cubic-bezier(0.4, 0, 1, 1)";

export function reducedMotion() {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia(REDUCED).matches
  );
}

/** A duration that becomes zero under reduced motion. */
export function motionMs(duration) {
  return reducedMotion() ? 0 : duration;
}

/**
 * Paint a colour scheme. The first paint and a repeat of the same scheme stay
 * instant; a real switch fades the large surfaces through the `theme-fade`
 * class from app.css.
 */
let themeTimer;
export function paintTheme(dark, initial = false) {
  if (typeof document === "undefined" || dark === null) return;
  const root = document.documentElement;
  const next = dark ? "dark" : "light";
  if (root.dataset.theme === next) return;
  if (!initial && root.dataset.theme) {
    root.classList.add("theme-fade");
    clearTimeout(themeTimer);
    themeTimer = setTimeout(() => root.classList.remove("theme-fade"), 280);
  }
  root.dataset.theme = next;
}

/**
 * Re-run a short entrance animation whenever the page key changes, without
 * rebuilding the page. Keeping the DOM means focus and an in-progress search
 * stay where the user left them.
 */
export function pageIn(node, key) {
  let current = key;
  let animation = null;

  const play = () => {
    if (reducedMotion() || typeof node.animate !== "function") return;
    animation?.cancel();
    animation = node.animate(
      [
        { opacity: 0, transform: "translateY(4px)" },
        { opacity: 1, transform: "none" },
      ],
      { duration: 180, easing: EASE_OUT },
    );
  };

  return {
    update(next) {
      if (next === current) return;
      current = next;
      play();
    },
    destroy() {
      animation?.cancel();
    },
  };
}

/**
 * A smooth, interruptible <details> fold that keeps the native element.
 *
 * The page search opens and closes folds by setting `open`, so replacing
 * <details> would break that lookup and the text it reads. The native toggle
 * is intercepted for pointer and keyboard activation and the height is
 * animated with the Web Animations API; a programmatic `open` change still
 * works and simply clears any leftover inline height.
 */
export function fold(node) {
  const summary = node.querySelector("summary");
  if (!summary) return;

  let animation = null;
  let mode = null;

  const summaryHeight = () => summary.getBoundingClientRect().height;
  // The details' own border stays visible when it is closed. Animating to
  // the summary alone would leave a 1px jump at the end.
  const collapsedHeight = () =>
    summaryHeight() + (node.offsetHeight - node.clientHeight);

  function clear() {
    animation = null;
    mode = null;
    node.style.height = "";
    node.style.overflow = "";
    node.classList.remove("closing");
  }

  function stop() {
    if (!animation) return;
    const height = node.getBoundingClientRect().height;
    animation.cancel();
    animation = null;
    node.style.height = `${height}px`;
    node.style.overflow = "hidden";
  }

  function play(from, to, done) {
    const duration = reducedMotion() ? 0 : 190;
    if (!duration || typeof node.animate !== "function") {
      done();
      clear();
      return;
    }
    node.style.overflow = "hidden";
    animation = node.animate([{ height: `${from}px` }, { height: `${to}px` }], {
      duration,
      easing: mode === "closing" ? EASE_IN : EASE_OUT,
    });
    animation.onfinish = () => {
      done();
      clear();
    };
  }

  function open() {
    const start = node.style.height
      ? node.getBoundingClientRect().height
      : collapsedHeight();
    mode = "opening";
    node.classList.remove("closing");
    node.open = true;
    // The full border-box height: the summary, every row inside, and the
    // border that scrollHeight leaves out.
    const end = node.scrollHeight + (node.offsetHeight - node.clientHeight);
    play(start, end, () => {});
  }

  function close() {
    const start = node.getBoundingClientRect().height;
    mode = "closing";
    node.classList.add("closing");
    play(start, collapsedHeight(), () => {
      node.open = false;
    });
  }

  function toggle() {
    if (mode === "opening") {
      stop();
      close();
    } else if (mode === "closing") {
      stop();
      open();
    } else if (node.open) {
      close();
    } else {
      open();
    }
  }

  function onClick(event) {
    if (event.button !== 0) return;
    if (!(event.target instanceof Element)) return;
    if (!event.target.closest("summary")) return;
    event.preventDefault();
    toggle();
  }

  // The search sets `open` directly. When that disagrees with a running
  // animation, drop the animation and let the new state stand.
  function onToggle() {
    if (!mode) {
      clear();
      return;
    }
    const expected = mode === "opening";
    if (node.open !== expected) {
      animation?.cancel();
      clear();
    }
  }

  node.addEventListener("click", onClick);
  node.addEventListener("toggle", onToggle);
  return {
    destroy() {
      animation?.cancel();
      node.removeEventListener("click", onClick);
      node.removeEventListener("toggle", onToggle);
      node.style.height = "";
      node.style.overflow = "";
    },
  };
}
