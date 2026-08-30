#!/usr/bin/env python3
"""Validation gate for infra/: container, compose, Kubernetes and Terraform hygiene.

infra/ is the part of the repository that decides how Migo runs in the world, and
its failure modes are quiet: an unpinned base image drifts under you, a committed
private key is committed forever, a container that runs as root or with a writable
host mount is a foothold nobody notices until it is used. This script is the
mechanical half of reviewing that directory — everything it checks is a fact that
can be verified without judgement.

The development compose stack is deliberately insecure in ways the server itself
refuses outside development (an ephemeral node key, a placeholder token key, the
local database password); those exact constants are allow-listed below, and every
other secret-shaped value is a finding.

Usage: python3 tools/scripts/infra-audit.py [--root PATH] [--infra PATH]
Exit code 0 = clean, 1 = at least one problem.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from math import log2
from pathlib import Path

import yaml

# The two development secrets the stack is allowed to carry (brief/README §Security),
# plus the well-known local database password. Anything else in a secret-named field
# or a URL credential is a finding.
ALLOWED_SECRET_VALUES = {"migo", "development-only-insecure-token-key"}
WEB_PORT = 19992  # the web client's fixed port; see Dockerfile.web / README.

SECRET_KEY_RE = re.compile(
    r"(?i)(?:^|[_.\-])(?:password|passwd|pwd|secret|token|api[_-]?key|apikey"
    r"|access[_-]?key|secret[_-]?key|private[_-]?key|credential|authorization"
    r"|auth[_-]?token)(?:$|[_.\-]?)"
)
PEM_RE = re.compile(r"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----")
URL_CRED_RE = re.compile(r"[a-z][a-z0-9+.\-]*://([^/@\s:]+):([^/@\s]+)@")
WORKLOAD_KINDS = {
    "Pod", "Deployment", "StatefulSet", "DaemonSet",
    "ReplicaSet", "Job", "CronJob", "ReplicationController",
}


class Audit:
    def __init__(self) -> None:
        self.problems: list[str] = []
        self.checks = 0

    def ok(self, label: str) -> None:
        self.checks += 1
        print(f"  ok    {label}")

    def fail(self, label: str, detail: str) -> None:
        self.checks += 1
        self.problems.append(f"{label}: {detail}")
        print(f"  FAIL  {label}\n        {detail}")

    def expect(self, cond: bool, label: str, detail: str) -> bool:
        if cond:
            self.ok(label)
        else:
            self.fail(label, detail)
        return cond


def shannon_entropy(s: str) -> float:
    if not s:
        return 0.0
    counts: dict[str, int] = {}
    for ch in s:
        counts[ch] = counts.get(ch, 0) + 1
    n = len(s)
    return -sum((c / n) * log2(c / n) for c in counts.values())


def is_interpolation(value: str) -> bool:
    """A ${VAR} / $VAR reference or a build-arg placeholder is not a literal secret."""
    return "${" in value or bool(re.fullmatch(r"\$[A-Za-z_][A-Za-z0-9_]*", value))


def high_entropy_secret(value: str) -> bool:
    """A value that looks generated rather than typed: a long token with real entropy."""
    v = value.strip().strip("'\"")
    if v in ALLOWED_SECRET_VALUES or is_interpolation(v):
        return False
    if re.fullmatch(r"[A-Za-z0-9+/=_\-]{24,}", v) and shannon_entropy(v) >= 3.5:
        return True
    return len(v) >= 20 and shannon_entropy(v) >= 4.0


def walk_scalars(node, path=""):
    """Yield (dotted-path, key, scalar-string) for every scalar in a parsed document."""
    if isinstance(node, dict):
        for k, v in node.items():
            child = f"{path}.{k}" if path else str(k)
            if isinstance(v, (dict, list)):
                yield from walk_scalars(v, child)
            elif v is not None:
                yield child, str(k), str(v)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            child = f"{path}[{i}]"
            if isinstance(v, (dict, list)):
                yield from walk_scalars(v, child)
            elif v is not None:
                yield child, "", str(v)


def load_yaml_docs(text: str) -> list:
    return [d for d in yaml.safe_load_all(text) if d is not None]


def image_tag_problem(ref: str) -> str | None:
    """Return a reason string if a registry image reference is not pinned, else None."""
    ref = ref.strip().strip("'\"")
    if not ref or is_interpolation(ref) and "${" in ref and ":" not in ref.split("}")[-1]:
        # An unresolved bare interpolation with no visible tag — cannot verify.
        return "image is an unresolved variable with no visible tag"
    if "@sha256:" in ref:
        return None  # digest-pinned
    last = ref.rsplit("/", 1)[-1]  # strip registry host / namespace
    if ":" not in last:
        return "no tag (defaults to :latest)"
    tag = last.rsplit(":", 1)[1]
    if tag == "latest":
        return "pinned to the moving :latest tag"
    return None


def dockerfile_from_problems(text: str) -> list[tuple[str, str]]:
    """FROM base images that are not pinned. Resolves ARG defaults and skips stage refs."""
    args: dict[str, str] = {}
    stages: set[str] = set()
    problems: list[tuple[str, str]] = []

    def substitute(token: str) -> str:
        token = re.sub(r"\$\{([A-Za-z0-9_]+)\}",
                       lambda m: args.get(m.group(1), m.group(0)), token)
        token = re.sub(r"\$([A-Za-z0-9_]+)",
                       lambda m: args.get(m.group(1), m.group(0)), token)
        return token

    for raw in text.splitlines():
        line = raw.strip()
        m = re.match(r"(?i)ARG\s+([A-Za-z0-9_]+)\s*=\s*(\S+)", line)
        if m:
            args[m.group(1)] = m.group(2)
            continue
        m = re.match(r"(?i)^FROM\s+(.*)$", line)
        if not m:
            continue
        rest = m.group(1)
        # Drop FROM options such as --platform=...
        tokens = [t for t in rest.split() if not t.startswith("--")]
        if not tokens:
            continue
        ref = tokens[0]
        alias = tokens[2] if len(tokens) >= 3 and tokens[1].lower() == "as" else None
        # A FROM that names an earlier build stage is not a registry image.
        if ref in stages:
            if alias:
                stages.add(alias)
            continue
        resolved = substitute(ref)
        problem = image_tag_problem(resolved)
        if problem:
            problems.append((resolved, problem))
        if alias:
            stages.add(alias)
    return problems


def pod_specs(doc: dict):
    """Yield (kind, pod-spec) for a workload document."""
    kind = doc.get("kind")
    if kind == "Pod":
        spec = doc.get("spec")
        if isinstance(spec, dict):
            yield kind, spec
    elif kind == "CronJob":
        spec = (((doc.get("spec") or {}).get("jobTemplate") or {}).get("spec") or {})
        tmpl = (spec.get("template") or {}).get("spec")
        if isinstance(tmpl, dict):
            yield kind, tmpl
    else:
        tmpl = ((doc.get("spec") or {}).get("template") or {}).get("spec")
        if isinstance(tmpl, dict):
            yield kind, tmpl


def published_ports(service: dict) -> list[tuple[str, str]]:
    """(host-port, container-port) pairs a compose service publishes to the host."""
    out: list[tuple[str, str]] = []
    for entry in service.get("ports", []) or []:
        if isinstance(entry, dict):
            pub = entry.get("published")
            tgt = entry.get("target")
            if pub is not None:
                out.append((str(pub), str(tgt)))
            continue
        text = str(entry).split("/", 1)[0]  # drop /tcp|/udp
        parts = text.split(":")
        if len(parts) == 1:
            out.append((parts[0], parts[0]))          # "8080" (host-assigned)
        elif len(parts) == 2:
            out.append((parts[0], parts[1]))          # "H:C"
        else:
            out.append((parts[-2], parts[-1]))        # "IP:H:C"
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parents[2]))
    ap.add_argument("--infra", default=None)
    args = ap.parse_args()
    root = Path(args.root)
    infra = Path(args.infra) if args.infra else root / "infra"

    a = Audit()
    try:
        shown = infra.resolve().relative_to(root.resolve())
    except ValueError:
        shown = infra

    yaml_files = sorted(p for p in infra.rglob("*") if p.suffix in (".yml", ".yaml"))
    json_files = sorted(infra.rglob("*.json"))
    dockerfiles = sorted(p for p in infra.rglob("Dockerfile*") if p.is_file())
    tf_files = sorted(infra.rglob("*.tf"))
    compose_files = [p for p in yaml_files if "compose" in p.name or "compose" in p.parent.name]

    print(f"audit {shown}  "
          f"({len(yaml_files)} yaml, {len(json_files)} json, "
          f"{len(dockerfiles)} dockerfile, {len(tf_files)} tf)\n")

    # --- 1. everything parses -------------------------------------------------
    print("parse")
    parsed: dict[Path, list] = {}
    parse_errors: list[str] = []
    for p in yaml_files:
        try:
            parsed[p] = load_yaml_docs(p.read_text(encoding="utf-8"))
        except yaml.YAMLError as e:
            parse_errors.append(f"{p.relative_to(infra)}: {str(e).splitlines()[0]}")
    for p in json_files:
        try:
            json.loads(p.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            parse_errors.append(f"{p.relative_to(infra)}: {e}")
    a.expect(not parse_errors,
             f"every YAML/JSON file under infra parses ({len(yaml_files) + len(json_files)} files)",
             f"{parse_errors[:5]}")

    # --- 2. no committed secrets (bar the allow-listed development constants) --
    print("\nsecrets")
    # A private key or a URL credential can be committed in any file, not only in a
    # structured config one (a stray .key/.pem/.env is the classic slip), so these two
    # scans read every readable file under infra rather than the parseable subset.
    scan_files = [p for p in infra.rglob("*")
                  if p.is_file() and p.stat().st_size <= 1_000_000]

    def read_text(path: Path) -> str:
        return path.read_text(encoding="utf-8", errors="ignore")

    pem_hits = sorted({str(p.relative_to(infra)) for p in scan_files
                       if PEM_RE.search(read_text(p))})
    a.expect(not pem_hits, "no PEM private key material is committed", f"{pem_hits}")

    secret_hits: list[str] = []
    for p in scan_files:
        for user, pw in URL_CRED_RE.findall(read_text(p)):
            if pw.strip("'\"") not in ALLOWED_SECRET_VALUES and not is_interpolation(pw):
                secret_hits.append(f"{p.relative_to(infra)}: URL credential {user}:***")
    for p, docs in parsed.items():
        for doc in docs:
            for path, key, value in walk_scalars(doc):
                if SECRET_KEY_RE.search(key) and high_entropy_secret(value):
                    secret_hits.append(f"{p.relative_to(infra)}: {path} looks like a real secret")
    a.expect(not secret_hits,
             "no hard-coded secret outside the allow-listed development constants",
             f"{secret_hits[:5]}")

    # --- 3. every container image is pinned -----------------------------------
    print("\nimages")
    compose_img: list[str] = []
    for p in compose_files:
        for doc in parsed.get(p, []):
            for name, svc in (doc.get("services") or {}).items():
                if isinstance(svc, dict) and "image" in svc:
                    reason = image_tag_problem(str(svc["image"]))
                    if reason:
                        compose_img.append(f"{p.name}:{name} -> {svc['image']} ({reason})")
    a.expect(not compose_img, "every compose service image is pinned to a fixed tag",
             f"{compose_img}")

    df_img: list[str] = []
    for p in dockerfiles:
        for ref, reason in dockerfile_from_problems(p.read_text(encoding="utf-8")):
            df_img.append(f"{p.name}: {ref} ({reason})")
    a.expect(not df_img, "every Dockerfile base image is pinned to a fixed tag", f"{df_img}")

    # Kubernetes documents (any parsed doc carrying a kind), excluding compose.
    k8s_docs: list[tuple[Path, dict]] = []
    for p, docs in parsed.items():
        if p in compose_files:
            continue
        for doc in docs:
            if isinstance(doc, dict) and doc.get("kind") and doc.get("apiVersion"):
                k8s_docs.append((p, doc))

    k8s_img: list[str] = []
    for p, doc in k8s_docs:
        for _kind, spec in pod_specs(doc):
            for c in (spec.get("containers") or []) + (spec.get("initContainers") or []):
                reason = image_tag_problem(str(c.get("image", "")))
                if reason:
                    k8s_img.append(f"{p.name}: {c.get('name')} -> {c.get('image')} ({reason})")

    # --- 4. Kubernetes workloads are hardened ---------------------------------
    print("\nkubernetes")
    workloads = [(p, doc) for p, doc in k8s_docs if doc.get("kind") in WORKLOAD_KINDS]
    hardening: list[str] = []
    for p, doc in workloads:
        for _kind, spec in pod_specs(doc):
            pod_nonroot = ((spec.get("securityContext") or {}).get("runAsNonRoot") is True)
            for c in (spec.get("containers") or []):
                cname = c.get("name", "?")
                res = c.get("resources") or {}
                if not res.get("requests"):
                    hardening.append(f"{p.name}:{cname} has no resources.requests")
                if not res.get("limits"):
                    hardening.append(f"{p.name}:{cname} has no resources.limits")
                sc = c.get("securityContext") or {}
                if not (pod_nonroot or sc.get("runAsNonRoot") is True):
                    hardening.append(f"{p.name}:{cname} does not set runAsNonRoot")
                if not c.get("readinessProbe"):
                    hardening.append(f"{p.name}:{cname} has no readinessProbe")
                if not c.get("livenessProbe"):
                    hardening.append(f"{p.name}:{cname} has no livenessProbe")
    a.expect(not k8s_img, "every Kubernetes container image is pinned to a fixed tag", f"{k8s_img}")
    a.expect(not hardening,
             f"every Kubernetes workload sets requests+limits, non-root, and both probes "
             f"({len(workloads)} workload doc(s))",
             f"{hardening[:6]}")

    # --- 5. no externally-exposed Service without an explaining comment -------
    exposed: list[str] = []
    for p, doc in k8s_docs:
        if doc.get("kind") != "Service":
            continue
        stype = (doc.get("spec") or {}).get("type")
        if stype not in ("LoadBalancer", "NodePort"):
            continue
        lines = p.read_text(encoding="utf-8").splitlines()
        justified = False
        for i, ln in enumerate(lines):
            if re.search(rf"type:\s*{stype}\b", ln):
                if "#" in ln:
                    justified = True
                for j in range(i - 1, -1, -1):
                    if not lines[j].strip():
                        continue
                    justified = justified or lines[j].lstrip().startswith("#")
                    break
        if not justified:
            exposed.append(f"{p.name}: Service type {stype} has no explaining comment")
    a.expect(not exposed,
             "every LoadBalancer/NodePort Service carries an explaining comment",
             f"{exposed}")

    # --- 6. no privileged / host-namespace / writable host mount --------------
    print("\nruntime privileges")
    dangerous: list[str] = []
    for p in compose_files:
        for doc in parsed.get(p, []):
            for name, svc in (doc.get("services") or {}).items():
                if not isinstance(svc, dict):
                    continue
                if svc.get("privileged") is True:
                    dangerous.append(f"{p.name}:{name} is privileged")
                if str(svc.get("network_mode", "")).startswith("host"):
                    dangerous.append(f"{p.name}:{name} uses host networking")
                if svc.get("pid") == "host" or svc.get("ipc") == "host":
                    dangerous.append(f"{p.name}:{name} shares a host namespace (pid/ipc)")
                for vol in svc.get("volumes", []) or []:
                    if isinstance(vol, dict):
                        if vol.get("type") == "bind" and not vol.get("read_only"):
                            dangerous.append(f"{p.name}:{name} writable host bind {vol.get('source')}")
                    else:
                        parts = str(vol).split(":")
                        src = parts[0]
                        mode = parts[2] if len(parts) >= 3 else ""
                        if src[:1] in ("/", ".", "~") and mode != "ro":
                            dangerous.append(f"{p.name}:{name} writable host bind {src}")
    for p, doc in k8s_docs:
        for _kind, spec in pod_specs(doc):
            if spec.get("hostNetwork") is True:
                dangerous.append(f"{p.name} uses hostNetwork")
            if spec.get("hostPID") is True or spec.get("hostIPC") is True:
                dangerous.append(f"{p.name} shares a host namespace (pid/ipc)")
            for c in (spec.get("containers") or []) + (spec.get("initContainers") or []):
                sc = c.get("securityContext") or {}
                if sc.get("privileged") is True:
                    dangerous.append(f"{p.name}:{c.get('name')} is privileged")
                if sc.get("allowPrivilegeEscalation") is True:
                    dangerous.append(f"{p.name}:{c.get('name')} allows privilege escalation")
            for v in spec.get("volumes", []) or []:
                if isinstance(v, dict) and v.get("hostPath"):
                    dangerous.append(f"{p.name} mounts hostPath {v['hostPath'].get('path')}")
    a.expect(not dangerous,
             "no privileged container, host namespace, or writable host mount",
             f"{dangerous[:6]}")

    # --- 7. Terraform: no plaintext creds, no unexplained open ingress --------
    print("\nterraform")
    tf_problems: list[str] = []
    for p in tf_files:
        lines = p.read_text(encoding="utf-8").splitlines()
        for i, ln in enumerate(lines):
            m = re.search(r'(?i)\b(password|secret|token|access_key|secret_key)\b\s*=\s*"([^"]+)"', ln)
            if m and not is_interpolation(m.group(2)) and m.group(2) not in ALLOWED_SECRET_VALUES:
                tf_problems.append(f"{p.name}:{i + 1} hard-coded {m.group(1)}")
            if "0.0.0.0/0" in ln:
                prev = lines[i - 1].lstrip() if i else ""
                if "#" not in ln and not prev.startswith("#"):
                    tf_problems.append(f"{p.name}:{i + 1} open ingress 0.0.0.0/0 without an explaining comment")
        # A variable whose name looks secret must be declared sensitive.
        for m in re.finditer(r'(?is)variable\s+"([^"]+)"\s*\{(.*?)\}', "\n".join(lines)):
            vname, body = m.group(1), m.group(2)
            if SECRET_KEY_RE.search(vname) and "sensitive = true" not in body.replace(" ", " "):
                if not re.search(r"sensitive\s*=\s*true", body):
                    tf_problems.append(f"{p.name}: variable {vname} is not marked sensitive = true")
    a.expect(not tf_problems,
             f"terraform has no plaintext credential, open ingress, or unmarked secret variable "
             f"({len(tf_files)} .tf file(s))",
             f"{tf_problems[:5]}")

    # --- 8. compose ports do not collide, and the web port is fixed -----------
    print("\ncompose ports")
    collisions: list[str] = []
    web_ok = True
    web_detail = "no web service found"
    for p in compose_files:
        for doc in parsed.get(p, []):
            seen: dict[str, str] = {}
            services = doc.get("services") or {}
            for name, svc in services.items():
                if not isinstance(svc, dict):
                    continue
                for host, _container in published_ports(svc):
                    if host in seen:
                        collisions.append(f"{p.name}: host port {host} used by both {seen[host]} and {name}")
                    else:
                        seen[host] = name
            if "web" in services:
                hosts = [h for h, _ in published_ports(services["web"])]
                web_ok = str(WEB_PORT) in hosts
                web_detail = f"web publishes {hosts}, expected {WEB_PORT}"
    a.expect(not collisions, "no two compose services publish the same host port", f"{collisions}")
    a.expect(web_ok, f"the web service publishes the fixed port {WEB_PORT}", web_detail)

    print(f"\n{a.checks} checks, {len(a.problems)} problem(s)")
    if a.problems:
        print("\nPROBLEMS")
        for pr in a.problems:
            print(f"  - {pr}")
        return 1
    print("clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
