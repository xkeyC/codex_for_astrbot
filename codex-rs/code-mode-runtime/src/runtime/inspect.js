// Fork addition: `inspect(...values)` shows values in the model's output:
// strings as text, image-like values as images, anything else as JSON.
(() => {
  const emitText = globalThis.text;
  const emitImage = globalThis.image;

  function isImageLike(value) {
    if (typeof value === "string") {
      return /^data:image\//i.test(value);
    }
    if (value === null || typeof value !== "object") {
      return false;
    }
    return (
      typeof value.image_url === "string" ||
      (value.type === "image" &&
        typeof value.data === "string" &&
        typeof value.mimeType === "string")
    );
  }

  function asText(value) {
    if (typeof value === "string") return value;
    if (value === undefined) return "undefined";
    if (typeof value === "bigint") return `${value}n`;
    if (typeof value === "function" || typeof value === "symbol") {
      return String(value);
    }
    try {
      const json = JSON.stringify(value, null, 2);
      if (json !== undefined) return json;
    } catch (_) {
      // Cyclic or otherwise unserializable: fall back below.
    }
    try {
      return String(value);
    } catch (_) {
      return Object.prototype.toString.call(value);
    }
  }

  globalThis.inspect = function inspect(...values) {
    for (const value of values) {
      if (isImageLike(value)) {
        try {
          emitImage(value);
          continue;
        } catch (_) {
          // Not an image the runtime accepts: show it as text.
        }
      }
      emitText(asText(value));
    }
  };
})();
