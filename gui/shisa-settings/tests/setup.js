// jsdom does not implement matchMedia, and the settings window reads the
// desktop's colour preference on mount.
if (typeof window !== "undefined" && !window.matchMedia) {
  window.matchMedia = (query) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent() {
      return false;
    },
  });
}

// Svelte transitions and the fold action animate through the Web Animations
// API, which jsdom does not implement. Finish each animation on the next
// microtask so the element settles at its final state instead of throwing.
if (typeof Element !== "undefined" && !Element.prototype.animate) {
  Element.prototype.animate = function () {
    const animation = {
      currentTime: 0,
      playState: "running",
      oncancel: null,
      onfinish: null,
      cancel() {
        this.playState = "idle";
      },
    };
    queueMicrotask(() => {
      if (animation.playState === "idle") return;
      animation.currentTime = 1;
      animation.playState = "finished";
      animation.onfinish?.();
    });
    return animation;
  };
  Element.prototype.getAnimations = function () {
    return [];
  };
}
