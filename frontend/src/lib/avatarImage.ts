/**
 * Browser-side avatar image pipeline: pick an image file, then resize it down to a small
 * square via a canvas so the stored data-URL stays tiny (~10–30KB) — we never persist the
 * full original. Uses a plain `<input type="file">` (reliable in the Tauri webview) rather
 * than the native dialog + fs-read, since we only need the bytes in-page to draw to canvas.
 */

/** Target edge for the resized avatar. */
const AVATAR_SIZE = 256;
/** JPEG quality for the resized output (keeps data-URLs small while staying crisp). */
const AVATAR_QUALITY = 0.85;

/** Open a native file picker filtered to images; resolves to the chosen File or null. */
export function pickImageFile(): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/*";
    input.onchange = () => resolve(input.files?.[0] ?? null);
    // If the dialog is dismissed there is no `change`; we simply never resolve a file,
    // which is fine — the caller is awaiting a user action with no timeout.
    input.click();
  });
}

/** Load a File into an HTMLImageElement via an object URL (revoked once decoded). */
function loadImage(file: File): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("Could not decode image"));
    };
    img.src = url;
  });
}

/**
 * Resize an image File to a centered AVATAR_SIZE square and return a JPEG data-URL.
 * Cover-crops (scales to fill, centers) so non-square inputs aren't distorted.
 */
export async function fileToAvatarDataUrl(file: File): Promise<string> {
  const img = await loadImage(file);
  const canvas = document.createElement("canvas");
  canvas.width = AVATAR_SIZE;
  canvas.height = AVATAR_SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas unavailable");

  // Cover-fit: scale to fill the square, center the overflow.
  const scale = Math.max(AVATAR_SIZE / img.width, AVATAR_SIZE / img.height);
  const w = img.width * scale;
  const h = img.height * scale;
  ctx.drawImage(img, (AVATAR_SIZE - w) / 2, (AVATAR_SIZE - h) / 2, w, h);

  return canvas.toDataURL("image/jpeg", AVATAR_QUALITY);
}

/** Pick → resize → data-URL in one step; null if the user cancels the picker. */
export async function pickAndResizeAvatar(): Promise<string | null> {
  const file = await pickImageFile();
  if (!file) return null;
  return fileToAvatarDataUrl(file);
}
