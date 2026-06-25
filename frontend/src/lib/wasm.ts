// Lazy loader for the wasm-compiled protocol core (built from crates/mesh-talk-wasm into
// src/wasm). A single shared init so the module is fetched + instantiated once; the dynamic
// import keeps it in its own chunk, so the desktop bundle never ships it.

let ready: Promise<typeof import("@/wasm/mesh_talk_wasm.js")> | null = null;

export function loadWasm() {
  if (!ready) {
    ready = import("@/wasm/mesh_talk_wasm.js").then(async (m) => {
      await m.default(); // instantiate; our #[wasm_bindgen(start)] then runs (panic hook)
      return m;
    });
  }
  return ready;
}
