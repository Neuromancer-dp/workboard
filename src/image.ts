export const MAX_IMG_PX = 1280;

export interface PreparedImage {
  dataUrl: string;
  b64: string;
}

/** Downscale to at most `maxPx` on the long side and re-encode as JPEG (quality .85) before anything is sent or stored. */
export async function prepareImage(file: Blob, maxPx = MAX_IMG_PX): Promise<PreparedImage> {
  const bmp = await createImageBitmap(file);
  try {
    const k = Math.min(1, maxPx / Math.max(bmp.width, bmp.height));
    const c = document.createElement('canvas');
    c.width = Math.max(1, Math.round(bmp.width * k));
    c.height = Math.max(1, Math.round(bmp.height * k));
    const g = c.getContext('2d');
    if (!g) throw new Error('canvas unavailable');
    g.fillStyle = '#fff'; // JPEG has no alpha; flatten transparent PNGs onto white
    g.fillRect(0, 0, c.width, c.height);
    g.drawImage(bmp, 0, 0, c.width, c.height);
    const dataUrl = c.toDataURL('image/jpeg', 0.85);
    return { dataUrl, b64: dataUrl.slice(dataUrl.indexOf(',') + 1) };
  } finally {
    bmp.close();
  }
}
