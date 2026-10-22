import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  containerName,
  dockerArgs,
  dockerStartArgs,
  parseContainerOptions,
  removeContainer,
  runContainer,
} from "../../app-previews/container.mjs";

function fixture() {
  const repo = mkdtempSync(join(tmpdir(), "cadence-container-"));
  mkdirSync(join(repo, "app-previews", "social-content"), { recursive: true });
  writeFileSync(
    join(repo, "app-previews", "social-content", "index.html"),
    "<!doctype html>",
  );
  return repo;
}

test("container preview parses an app, port and local image", () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      [
        "--app",
        "social-content",
        "--port",
        "3195",
        "--image",
        "local/runtime:test",
      ],
      repo,
    );
    assert.equal(options.app, "social-content");
    assert.equal(options.port, 3195);
    assert.equal(options.image, "local/runtime:test");
    assert.equal(options.repo, repo);
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("container preview refuses unsafe or incomplete inputs", () => {
  const repo = fixture();
  try {
    for (const args of [
      ["--app", "../social-content", "--image", "local/runtime:test"],
      ["--app", "social-content", "--port", "3010", "--image", "local/runtime:test"],
      ["--app", "social-content", "--port", "3200", "--image", "local/runtime:test"],
      ["--app", "social-content"],
      ["--app", "social-content", "--image", ""],
      ["--app", "social-content", "--image", "-bad"],
      ["--app", "social-content", "--image", "local/runtime:test", "--backend", "x"],
    ]) {
      assert.throws(
        () => parseContainerOptions(args, repo),
        undefined,
        JSON.stringify(args),
      );
    }
    assert.throws(
      () =>
        parseContainerOptions(
          ["--app", "missing", "--image", "local/runtime:test"],
          repo,
        ),
      /index\.html/,
    );
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("container names differ by checkout and port", () => {
  const a = fixture();
  const b = fixture();
  try {
    const one = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      a,
    );
    const two = parseContainerOptions(
      ["--port", "3196", "--image", "local/runtime:test"],
      a,
    );
    const other = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      b,
    );
    assert.notEqual(containerName(one), containerName(two));
    assert.notEqual(containerName(one), containerName(other));
  } finally {
    rmSync(a, { recursive: true, force: true });
    rmSync(b, { recursive: true, force: true });
  }
});

test("docker args mount only the checkout and publish loopback", () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const args = dockerArgs(options);
    assert.equal(args[0], "create");
    assert.ok(!args.includes("--rm"));
    assert.ok(args.includes("--pull"));
    assert.ok(args.includes("never"));
    assert.ok(args.includes(`127.0.0.1:3195:3195`));
    assert.ok(args.includes(`${repo}:/work`));
    assert.ok(args.includes("/work"));
    assert.ok(args.includes("--container"));
    assert.equal(args.at(-1), "true");
    assert.ok(!args.some((arg) => arg.includes("docker.sock")));
    assert.ok(!args.some((arg) => arg.includes(".cadence/")));
    assert.ok(!args.some((arg) => arg.includes("CADENCE_PM_DIR=/root")));
    assert.deepEqual(dockerStartArgs("abc123"), ["start", "--attach", "abc123"]);
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("removeContainer force-removes the named container", async () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const calls = [];
    const removed = await removeContainer(options, (command, args, callback) => {
      calls.push({ command, args });
      callback(null);
    });
    assert.equal(removed, true);
    assert.deepEqual(calls[0], {
      command: "docker",
      args: ["rm", "-f", containerName(options)],
    });
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("removeContainer force-removes an explicit container id", async () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const calls = [];
    const removed = await removeContainer(
      options,
      (command, args, callback) => {
        calls.push({ command, args });
        callback(new Error("No such container"));
      },
      "abc123",
    );
    assert.equal(removed, true);
    assert.deepEqual(calls[0], {
      command: "docker",
      args: ["rm", "-f", "abc123"],
    });
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("runContainer creates, then starts one docker process", async () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const calls = [];
    const child = new EventEmitter();
    const spawn = (command, args, config) => {
      calls.push({ command, args, config });
      queueMicrotask(() => child.emit("exit", 0, null));
      return child;
    };
    const execFile = (command, args, callback) => {
      calls.push({ command, args });
      callback(null, args[0] === "create" ? "container-id\n" : "");
    };
    const result = await runContainer(options, {
      spawn,
      execFile,
      probePort: async () => {},
    });
    assert.deepEqual(result, { code: 0, signal: null });
    assert.equal(calls[0].command, "docker");
    assert.deepEqual(calls[0].args.slice(0, 2), ["version", "--format"]);
    assert.equal(calls[1].command, "docker");
    assert.deepEqual(calls[1].args.slice(0, 3), ["image", "inspect", "--format"]);
    assert.deepEqual(calls[2].args, ["rm", "-f", containerName(options)]);
    assert.equal(calls[3].args[0], "create");
    assert.equal(calls[4].command, "docker");
    assert.deepEqual(calls[4].args, ["start", "--attach", "container-id"]);
    assert.equal(calls[4].config.stdio, "inherit");
    assert.deepEqual(calls[5].args, ["rm", "-f", "container-id"]);
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("a signal after create removes the container and never starts it", async () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const calls = [];
    const execFile = (command, args, callback) => {
      calls.push(args);
      if (args[0] === "create") {
        callback(null, "container-id\n");
        process.emit("SIGTERM");
        return;
      }
      callback(null, "");
    };
    const result = await runContainer(options, {
      execFile,
      spawn: () => {
        throw new Error("must not start after a signal");
      },
      probePort: async () => {},
    });
    assert.deepEqual(result, { code: 143, signal: "SIGTERM" });
    assert.deepEqual(
      calls.map((args) => args[0]),
      ["version", "image", "rm", "create", "rm"],
    );
    assert.deepEqual(calls.at(-1).slice(0, 3), ["rm", "-f", "container-id"]);
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});

test("runContainer refuses a missing local image before spawning", async () => {
  const repo = fixture();
  try {
    const options = parseContainerOptions(
      ["--port", "3195", "--image", "local/runtime:test"],
      repo,
    );
    const calls = [];
    const execFile = (command, args, callback) => {
      calls.push({ command, args });
      callback(args[0] === "image" ? new Error("missing") : null);
    };
    await assert.rejects(
      runContainer(options, {
        execFile,
        spawn: () => {
          throw new Error("must not spawn");
        },
        probePort: async () => {
          throw new Error("must not probe");
        },
      }),
      /not available locally/,
    );
    assert.equal(calls.length, 2);
    assert.deepEqual(calls[1].args.slice(0, 2), ["image", "inspect"]);
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
});
