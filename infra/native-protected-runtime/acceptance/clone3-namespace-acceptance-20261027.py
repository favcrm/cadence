#!/usr/bin/env python3
"""Independent real-Docker CAD-1085 clone3/namespace enforcement acceptance."""

import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import uuid

POLICY = Path(__file__).resolve().parents[1] / "seccomp.json"
SAFE_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
UID = 60001
LABEL = "cadence.acceptance.cad1085.clone3"

C_SOURCE = r'''
#define _GNU_SOURCE
#include <errno.h>
#include <linux/sched.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#if !defined(__x86_64__) || defined(__ILP32__)
#error This acceptance requires native amd64, not a compatibility ABI.
#endif
#ifndef SYS_clone3
#error The host headers must expose the real clone3 syscall number.
#endif

static void expect_errno(const char *label, long result, int wanted) {
    int actual = errno;
    if (result != -1 || actual != wanted) {
        fprintf(stderr, "%s: result=%ld errno=%d expected_errno=%d\n",
                label, result, actual, wanted);
        exit(2);
    }
}

static long checked_clone(unsigned long flags) {
    long result = syscall(SYS_clone, flags, NULL, NULL, NULL, 0UL);
    if (result == 0) _exit(86);
    if (result > 0) {
        kill((pid_t)result, SIGKILL);
        waitpid((pid_t)result, NULL, 0);
    }
    return result;
}

static long checked_clone3(struct clone_args *args) {
    long result = syscall(SYS_clone3, args, sizeof(*args));
    if (result == 0) _exit(87);
    if (result > 0) {
        kill((pid_t)result, SIGKILL);
        waitpid((pid_t)result, NULL, 0);
    }
    return result;
}

static void *thread_body(void *unused) {
    (void)unused;
    return (void *)(uintptr_t)42;
}

int main(int argc, char **argv) {
    if (argc != 2) return 3;
    int candidate = strcmp(argv[1], "candidate") == 0;
    if (!candidate && strcmp(argv[1], "control") != 0) return 3;
    if (getuid() != 60001 || geteuid() != 60001 || getgid() != 60001)
        return 4;

    errno = 0;
    expect_errno("clone3-null", syscall(SYS_clone3, NULL, 0),
                 candidate ? ENOSYS : EINVAL);

    const struct {
        const char *name;
        unsigned long flag;
    } namespaces[] = {
        {"NEWNS", CLONE_NEWNS},
        {"NEWCGROUP", CLONE_NEWCGROUP},
        {"NEWUTS", CLONE_NEWUTS},
        {"NEWIPC", CLONE_NEWIPC},
        {"NEWUSER", CLONE_NEWUSER},
        {"NEWPID", CLONE_NEWPID},
        {"NEWNET", CLONE_NEWNET}
    };
    unsigned long combined = 0;
    for (size_t i = 0; i < sizeof(namespaces) / sizeof(namespaces[0]); ++i) {
        char label[96];
        unsigned long flag = namespaces[i].flag;
        combined |= flag;
        snprintf(label, sizeof(label), "clone-invalid-%s", namespaces[i].name);
        errno = 0;
        expect_errno(label, checked_clone(flag | CLONE_THREAD | SIGCHLD),
                     candidate ? EPERM : EINVAL);
        snprintf(label, sizeof(label), "unshare-invalid-%s", namespaces[i].name);
        errno = 0;
        expect_errno(label, syscall(SYS_unshare, flag | 0x80000000UL),
                     candidate ? EPERM : EINVAL);
        if (candidate) {
            snprintf(label, sizeof(label), "clone-namespace-%s", namespaces[i].name);
            errno = 0;
            expect_errno(label, checked_clone(flag | SIGCHLD), EPERM);
            snprintf(label, sizeof(label), "unshare-namespace-%s", namespaces[i].name);
            errno = 0;
            expect_errno(label, syscall(SYS_unshare, flag), EPERM);
        }
    }
    errno = 0;
    expect_errno("clone-invalid-combined",
                 checked_clone(combined | CLONE_THREAD | SIGCHLD),
                 candidate ? EPERM : EINVAL);
    errno = 0;
    expect_errno("setns-invalid-fd", syscall(SYS_setns, -1, CLONE_NEWNET),
                 candidate ? EPERM : EBADF);
    errno = 0;
    long unshare_zero = syscall(SYS_unshare, 0UL);
    if (candidate) {
        expect_errno("unshare-zero", unshare_zero, EPERM);
    } else if (unshare_zero != 0) {
        fprintf(stderr, "control unshare(0) did not succeed: errno=%d\n", errno);
        return 5;
    }
    if (candidate) {
        struct clone_args args = {0};
        args.flags = CLONE_NEWNET;
        args.exit_signal = SIGCHLD;
        errno = 0;
        expect_errno("clone3-valid-namespace", checked_clone3(&args), ENOSYS);
    }

    pthread_t thread;
    void *result = NULL;
    int created = pthread_create(&thread, NULL, thread_body, NULL);
    if (created != 0) {
        fprintf(stderr, "pthread_create failed: %d\n", created);
        return 6;
    }
    int joined = pthread_join(thread, &result);
    if (joined != 0 || (uintptr_t)result != 42) {
        fprintf(stderr, "pthread_join/result failed: %d\n", joined);
        return 7;
    }
    puts(candidate ? "CAD1085_SECCOMP_CANDIDATE_OK" :
                     "CAD1085_SECCOMP_CONTROL_OK");
    return 0;
}
'''

NODE_SOURCE = r'''
'use strict';
const fs = require('node:fs');
const crypto = require('node:crypto');
const { Worker } = require('node:worker_threads');
const actual = crypto.createHash('sha256')
  .update(fs.readFileSync(process.execPath)).digest('hex');
if (actual !== process.argv[1] || process.getuid() !== 60001 ||
    process.getgid() !== 60001) process.exit(90);
let received = false;
const worker = new Worker(
  "require('node:worker_threads').parentPort.postMessage(42)",
  { eval: true }
);
worker.once('error', () => process.exit(91));
worker.once('message', value => {
  if (value !== 42 || received) process.exit(92);
  received = true;
});
worker.once('exit', code => {
  if (code !== 0 || !received) process.exit(93);
  console.log('CAD1085_NODE_THREAD_OK');
});
'''


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scratch-root", required=True, type=Path)
    parser.add_argument("--image", required=True)
    parser.add_argument("--node-sha256", required=True)
    parser.add_argument("--policy-sha256", required=True)
    args = parser.parse_args()
    require(args.scratch_root.is_absolute(), "scratch root must be an absolute path")
    require(platform.system() == "Linux" and platform.machine() == "x86_64",
            "requires a native Linux amd64 host")
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", args.image) is not None,
            "image must be an already loaded immutable sha256 image ID")
    for value in (args.node_sha256, args.policy_sha256):
        require(re.fullmatch(r"[0-9a-f]{64}", value) is not None,
                "node/policy pins must be lowercase 64-hex SHA256")
    metadata = args.scratch_root.lstat()
    require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == os.geteuid()
            and metadata.st_mode & 0o077 == 0,
            "artifact root must be an existing private directory owned by caller")
    policy_bytes = POLICY.read_bytes()
    policy_sha = hashlib.sha256(policy_bytes).hexdigest()
    require(policy_sha == args.policy_sha256, "reviewed candidate policy pin changed")
    docker = shutil.which("docker", path=SAFE_PATH)
    compiler = shutil.which("cc", path="/usr/bin:/bin")
    require(docker is not None and compiler is not None, "Docker and trusted cc required")
    token = uuid.uuid4().hex

    with tempfile.TemporaryDirectory(prefix="seccomp-acceptance-", dir=args.scratch_root) as root:
        root = Path(root)
        config = root / "docker-config"
        config.mkdir(mode=0o700)
        (config / "config.json").write_text('{"auths":{}}\n')
        environment = {
            "PATH": SAFE_PATH, "LC_ALL": "C", "HOME": str(root),
            "DOCKER_CONFIG": str(config),
        }
        docker_prefix = [docker, "--host", "unix:///var/run/docker.sock"]

        def run(command, timeout=30, allow_failure=False):
            result = subprocess.run(command, env=environment, capture_output=True,
                                    text=True, timeout=timeout)
            if not allow_failure and result.returncode != 0:
                raise RuntimeError(
                    f"subprocess failed rc={result.returncode}: {result.stderr[:3000]}"
                )
            return result

        info = json.loads(run(docker_prefix + ["image", "inspect", args.image]).stdout)
        require(len(info) == 1 and info[0]["Id"] == args.image,
                "loaded image does not match immutable image ID")
        require(info[0].get("Architecture") == "amd64" and info[0].get("Os") == "linux",
                "fixture image must be native Linux amd64")
        image_config = info[0].get("Config") or {}
        require(not image_config.get("Volumes"), "fixture image must not create volumes")
        safe_image_env = {
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "PATH=/opt/cadence/node/bin:/usr/bin:/bin",
            "HOSTNAME=cadence-protected", "HOME=/home/agent",
            "PI_CODING_AGENT_DIR=/home/agent/pi",
        }
        inherited = image_config.get("Env") or []
        require(all(entry in safe_image_env for entry in inherited),
                "fixture image contains unapproved inherited environment")
        require(len({entry.split("=", 1)[0] for entry in inherited}) == len(inherited),
                "fixture image contains duplicate environment keys")

        candidate = root / "candidate.json"
        candidate.write_bytes(policy_bytes)
        source = root / "witness.c"
        source.write_text(C_SOURCE)
        witness = root / "witness"
        run([compiler, "-static", "-O2", "-Wall", "-Wextra", "-Werror", "-pthread",
             "-o", str(witness), str(source)], timeout=90)
        witness.chmod(0o555)
        names = []

        def container(kind, entrypoint, arguments, security):
            name = f"cad1085-seccomp-acceptance-{token}-{kind}"
            names.append(name)
            command = docker_prefix + [
                "run", "--rm", "--pull=never", "--name", name,
                "--label", f"{LABEL}={token}", "--network=none", "--read-only",
                "--cap-drop=ALL", "--security-opt=no-new-privileges:true",
                "--security-opt", security, "--user", f"{UID}:{UID}",
                "--pids-limit=64", "--memory=256m", "--cpus=1",
                "--ulimit=core=0:0", "--ulimit=nofile=1024:1024",
                "--stop-timeout=1", "--no-healthcheck", "--workdir=/workspace",
                "--tmpfs", f"/home/agent:rw,nosuid,nodev,uid={UID},gid={UID},mode=0700,size=33554432",
                "--tmpfs", f"/workspace:rw,nosuid,nodev,uid={UID},gid={UID},mode=0700,size=33554432",
                "--tmpfs", "/tmp:rw,nosuid,nodev,size=33554432",
                "--tmpfs", "/run:rw,nosuid,nodev,size=33554432",
                "--env", "PATH=/opt/cadence/node/bin:/usr/bin:/bin",
                "--env", "HOSTNAME=cadence-protected",
                "--env", "HOME=/home/agent",
                "--env", "PI_CODING_AGENT_DIR=/home/agent/pi",
                "--mount", f"type=bind,src={witness},dst=/cad1085-seccomp-witness,readonly",
                "--entrypoint", entrypoint, args.image,
            ] + arguments
            return run(command).stdout.strip()

        try:
            control = container("control", "/cad1085-seccomp-witness", ["control"],
                                "seccomp=unconfined")
            require(control == "CAD1085_SECCOMP_CONTROL_OK",
                    "kernel discrimination control failed or emitted unexpected output")
            guarded = container("candidate", "/cad1085-seccomp-witness", ["candidate"],
                                f"seccomp={candidate}")
            require(guarded == "CAD1085_SECCOMP_CANDIDATE_OK",
                    "actual candidate syscall/thread assertions did not all complete")
            node = container("node", "/opt/cadence/node/bin/node",
                             ["--eval", NODE_SOURCE, args.node_sha256],
                             f"seccomp={candidate}")
            require(node == "CAD1085_NODE_THREAD_OK",
                    "actual pinned Node worker thread did not complete")
        finally:
            for name in reversed(names):
                inspected = run(docker_prefix + ["container", "inspect", name],
                                allow_failure=True)
                if inspected.returncode != 0:
                    error = inspected.stderr.lower()
                    require("no such object" in error or "no such container" in error,
                            "cannot establish owned-container cleanup state")
                    continue
                objects = json.loads(inspected.stdout)
                require(len(objects) == 1 and
                        (objects[0].get("Config", {}).get("Labels") or {}).get(LABEL) == token,
                        "refusing cleanup of an unowned container")
                run(docker_prefix + ["container", "rm", "--force", name])
    print(f"PASS CAD1085 clone3/namespace/thread policy_sha256={policy_sha} "
          f"image={args.image} node_sha256={args.node_sha256}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"FAIL CAD1085 clone3 acceptance: {error}", file=sys.stderr)
        sys.exit(1)
