// A fresh classic worker owns one Emscripten command and its in-memory files.
self.onmessage = async ({ data }) => {
  const { file, programUrl, wasmUrl, command, primaryIndex, imageCount } = data;
  let finished = false;
  let output = "";
  let outputTooLarge = false;
  const finish = (message) => {
    if (finished) return;
    finished = true;
    self.postMessage(message);
  };
  try {
    const input = new Uint8Array(await file.arrayBuffer());
    const module = {
      noInitialRun: true,
      locateFile: (name) => name.endsWith(".wasm") ? wasmUrl : name,
      print: (line) => {
        if (output.length + line.length > 64 * 1024) outputTooLarge = true;
        else output += `${line}\n`;
      },
      printErr: () => {},
      onAbort: () => finish({ error: true }),
      onRuntimeInitialized() {
        try {
          module.FS.writeFile("/photo.heic", input);
          const args = command === "info"
            ? ["/photo.heic"]
            : ["--transparency-composition-mode", "white", "-q", "95", "/photo.heic", "/photo.jpg"];
          const result = module.callMain(args);
          if (result !== 0 || outputTooLarge) throw new Error("HEIF command failed");
          if (command === "info") {
            finish({ output });
          } else {
            const generated = module.FS.readdir("/").filter((name) => /^photo(?:-\d+)?\.jpg$/.test(name));
            if (generated.length !== imageCount) throw new Error("HEIF image count changed");
            const path = imageCount === 1 ? "/photo.jpg" : `/photo-${primaryIndex + 1}.jpg`;
            const bytes = module.FS.readFile(path);
            finish({ blob: new Blob([bytes], { type: "image/jpeg" }) });
          }
        } catch {
          finish({ error: true });
        }
      },
    };
    self.Module = module;
    importScripts(programUrl);
  } catch {
    finish({ error: true });
  }
};
