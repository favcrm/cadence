// The AgenticOS gateway applies these bounds to a complete model request.
export const MAX_IMAGE_BASE64_CHARS = 192 * 1024;
export const MAX_IMAGE_PARTS = 4;

/** The gateway's refusal code when the resolved model cannot take image input. */
export const IMAGE_INPUT_UNAVAILABLE = "image_input_unavailable";
