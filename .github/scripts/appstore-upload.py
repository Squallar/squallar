#!/usr/bin/env python3
"""Upload an .ipa to App Store Connect from Linux, over HTTPS and nothing else.

This exists because the tools everyone reaches for cannot run here. `xcrun
altool` and `notarytool` ship inside Xcode and are macOS-only. Apple's
Transporter does have a Red Hat build, but its own error on this platform is
"Unable to perform software analysis on Linux. Export an AppStoreInfo.plist
from Xcode, and use the -assetDescription option" -- an input that, by name,
comes off a Mac.

None of that is on the path this script takes. App Store Connect API 4.1 added
the `buildUploads` and `buildUploadFiles` resources, whose release note reads
"You can now use Build uploads to upload and manage build uploads for your
apps", and the WWDC25 session that introduced them says the point of the design
in as many words: "you can upload your build using any language or platform you
prefer". It is a REST flow with a bearer token, so it needs an HTTP client and
an ECDSA signature and no Apple software at all.

    POST  /v1/buildUploads       version + platform + the app  -> an id
    POST  /v1/buildUploadFiles   file name + size + type       -> uploadOperations
    PUT   <each operation's url> the bytes, possibly in chunks
    PATCH /v1/buildUploadFiles/<id>  uploaded: true            -> the upload commits
    GET   /v1/buildUploads/<id>  poll until it stops being IN_PROGRESS

Dependencies are the standard library plus `openssl`. Python has no ECDSA in
the stdlib, so the one primitive that needs a real crypto implementation -- the
ES256 signature over the JWT -- is shelled out to `openssl dgst`, and the
DER-to-JOSE conversion of its output is done here, because that is arithmetic
rather than cryptography.

Run with --rehearse to exercise everything that does not need Apple: key
loading, JWT assembly, the DER->JOSE conversion (verified against openssl's own
verifier), and reading the version fields out of a real .ipa.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import plistlib
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

API = "https://api.appstoreconnect.apple.com"
# Apple's own name for the audience; a token minted for anything else is
# rejected as unauthorized with no further explanation.
AUDIENCE = "appstoreconnect-v1"
# Apple caps App Store Connect tokens at 20 minutes. A whole upload can outlast
# that, so the token is re-minted per request rather than once per run; signing
# one costs a single openssl invocation.
TOKEN_LIFETIME_S = 20 * 60


def fail(msg: str) -> "None":
    """Exit non-zero with a message the CI log will surface as an annotation."""
    print(f"::error::{msg}" if os.environ.get("GITHUB_ACTIONS") else f"ERROR: {msg}",
          file=sys.stderr)
    sys.exit(1)


def note(msg: str) -> None:
    print(msg, flush=True)


# ---------------------------------------------------------------- JWT (ES256)


def _b64(data: bytes) -> str:
    """base64url, unpadded -- the only encoding JWT accepts."""
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def der_to_jose(der: bytes) -> bytes:
    """Convert an ECDSA signature from openssl's DER to JWS's raw r||s.

    openssl emits SEQUENCE { INTEGER r, INTEGER s }. JOSE wants the two values
    concatenated, each left-padded to the coordinate size -- 32 bytes for
    P-256. The lengths differ run to run, because a leading zero byte appears
    in DER whenever the high bit of the integer is set and is absent otherwise,
    so a signature that happens to be 70 bytes today and 71 tomorrow is normal
    and is exactly what this function is for.
    """
    if len(der) < 8 or der[0] != 0x30:
        raise ValueError("not a DER SEQUENCE")
    # A P-256 signature is always short-form length; anything else is not ours.
    if der[1] & 0x80:
        raise ValueError("unexpected long-form DER length")
    body, out = der[2:2 + der[1]], b""
    for _ in range(2):
        if not body or body[0] != 0x02:
            raise ValueError("expected a DER INTEGER")
        n = body[1]
        v = body[2:2 + n].lstrip(b"\x00")
        if len(v) > 32:
            raise ValueError("integer wider than P-256")
        out += v.rjust(32, b"\x00")
        body = body[2 + n:]
    return out


def sign_es256(payload: bytes, key_path: str) -> bytes:
    """SHA-256 ECDSA over `payload` with the private key at `key_path`."""
    p = subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", key_path],
        input=payload, capture_output=True,
    )
    if p.returncode != 0:
        raise RuntimeError(f"openssl could not sign with {key_path}: "
                           f"{p.stderr.decode(errors='replace').strip()}")
    return der_to_jose(p.stdout)


def mint_token(issuer_id: str, key_id: str, key_path: str) -> str:
    """A signed App Store Connect bearer token.

    `iss` carries the issuer ID because these are team keys; an individual key
    identifies itself with `sub` instead and is not what this pipeline uses.
    """
    now = int(time.time())
    header = {"alg": "ES256", "kid": key_id, "typ": "JWT"}
    claims = {"iss": issuer_id, "iat": now, "exp": now + TOKEN_LIFETIME_S,
              "aud": AUDIENCE}
    signing_input = ".".join(
        _b64(json.dumps(x, separators=(",", ":")).encode()) for x in (header, claims)
    ).encode("ascii")
    return f"{signing_input.decode('ascii')}.{_b64(sign_es256(signing_input, key_path))}"


# ------------------------------------------------------------------ the calls


class Client:
    def __init__(self, issuer_id: str, key_id: str, key_path: str) -> None:
        self._cred = (issuer_id, key_id, key_path)

    def _request(self, method: str, url: str, body: object = None,
                 raw: bytes | None = None, headers: dict | None = None) -> dict:
        data = raw if raw is not None else (
            json.dumps(body).encode() if body is not None else None)
        h = dict(headers or {})
        if raw is None:
            # The pre-signed blob-store URLs are unauthenticated by design and
            # must not be sent a bearer token; every api.appstoreconnect.apple.com
            # call must carry one.
            h["Authorization"] = f"Bearer {mint_token(*self._cred)}"
            if body is not None:
                h["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, method=method, headers=h)
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                payload = r.read()
        except urllib.error.HTTPError as e:
            detail = e.read().decode(errors="replace")
            # Apple returns a JSON errors array; printing the whole body rather
            # than the status alone is the difference between "422" and the
            # sentence naming the attribute that was wrong.
            raise ApiError(e.code, f"{method} {url} -> HTTP {e.code}\n{detail}") from None
        except urllib.error.URLError as e:
            raise RuntimeError(f"{method} {url} failed to connect: {e.reason}") from None
        return json.loads(payload) if payload else {}

    def get(self, path: str) -> dict:
        return self._request("GET", API + path)

    def post(self, path: str, body: object) -> dict:
        return self._request("POST", API + path, body=body)

    def patch(self, path: str, body: object) -> dict:
        return self._request("PATCH", API + path, body=body)

    def put_bytes(self, url: str, chunk: bytes, headers: dict) -> None:
        self._request("PUT", url, raw=chunk, headers=headers)


class ApiError(RuntimeError):
    def __init__(self, status: int, msg: str) -> None:
        super().__init__(msg)
        self.status = status


# -------------------------------------------------------------- reading the ipa


def ipa_metadata(path: str) -> tuple[str, str, str]:
    """(bundle id, CFBundleShortVersionString, CFBundleVersion) from the .ipa.

    Read out of the archive rather than passed in, so the three values App
    Store Connect is told about are by construction the ones inside the binary
    it is being told about.
    """
    with zipfile.ZipFile(path) as z:
        names = [n for n in z.namelist()
                 if re.fullmatch(r"Payload/[^/]+\.app/Info\.plist", n)]
        if len(names) != 1:
            fail(f"{path} holds {len(names)} Payload/*.app/Info.plist entries; "
                 "expected exactly one.")
        pl = plistlib.loads(z.read(names[0]))
    missing = [k for k in ("CFBundleIdentifier", "CFBundleShortVersionString",
                           "CFBundleVersion") if not pl.get(k)]
    if missing:
        fail(f"{names[0]} is missing {', '.join(missing)}.")
    return (pl["CFBundleIdentifier"], pl["CFBundleShortVersionString"],
            pl["CFBundleVersion"])


def md5_of(path: str) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


# ------------------------------------------------------------------- the flow


def upload(client: Client, ipa: str, platform: str, timeout_s: int) -> int:
    bundle_id, short_version, build_version = ipa_metadata(ipa)
    size = os.path.getsize(ipa)
    note(f"==> {os.path.basename(ipa)}: {bundle_id} {short_version} "
         f"({build_version}), {size} bytes")

    # The app is found by bundle id rather than configured as a secret: the id
    # is already in the .ipa, and one fewer hand-copied value is one fewer way
    # to upload a build to the wrong app.
    apps = client.get(f"/v1/apps?filter[bundleId]={bundle_id}").get("data", [])
    if not apps:
        fail(f"App Store Connect has no app with bundle id {bundle_id} visible to "
             "this API key. Create the app record in App Store Connect first -- "
             "the upload API attaches builds to an app that already exists, it "
             "does not create one.")
    if len(apps) > 1:
        fail(f"{len(apps)} apps match bundle id {bundle_id}; refusing to guess.")
    app_id = apps[0]["id"]
    note(f"    app {app_id}")

    upload_id = client.post("/v1/buildUploads", {
        "data": {
            "type": "buildUploads",
            "attributes": {
                "cfBundleShortVersionString": short_version,
                "cfBundleVersion": build_version,
                "platform": platform,
            },
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}},
        }
    })["data"]["id"]
    note(f"    buildUpload {upload_id}")

    reserved = client.post("/v1/buildUploadFiles", {
        "data": {
            "type": "buildUploadFiles",
            "attributes": {
                "fileName": os.path.basename(ipa),
                "fileSize": size,
                "assetType": "ASSET",
                "uti": "com.apple.ipa",
            },
            "relationships": {
                "buildUpload": {"data": {"type": "buildUploads", "id": upload_id}}
            },
        }
    })["data"]
    file_id = reserved["id"]
    ops = reserved["attributes"].get("uploadOperations") or []
    if not ops:
        fail("the reservation came back with no uploadOperations, so there is "
             "nowhere to send the binary.")
    note(f"    buildUploadFile {file_id}: {len(ops)} upload operation(s)")

    # Apple hands back one operation per chunk for a large file, each with its
    # own byte range. Sent in order and one at a time: they may legally go in
    # parallel, but a CI runner's uplink is the bottleneck either way and a
    # serial loop is one that reports which chunk failed.
    with open(ipa, "rb") as f:
        for i, op in enumerate(ops, 1):
            f.seek(op["offset"])
            chunk = f.read(op["length"])
            if len(chunk) != op["length"]:
                fail(f"operation {i} wants {op['length']} bytes at offset "
                     f"{op['offset']}, but the file ended early.")
            headers = {h["name"]: h["value"] for h in op.get("requestHeaders", [])}
            client.put_bytes(op["url"], chunk, headers)
            note(f"    chunk {i}/{len(ops)}: {len(chunk)} bytes at {op['offset']}")

    # Two Apple documents disagree about this request. The generic asset flow
    # ("Uploading Assets to App Store Connect") requires `sourceFileChecksum`
    # beside `uploaded`; the WWDC25 walkthrough of this specific resource shows
    # `uploaded` alone. Send both, because the checksum is the only end-to-end
    # check that the bytes arrived intact -- and if this resource rejects the
    # attribute, say so and commit without it rather than failing an upload
    # over a documentation ambiguity.
    commit = {"data": {"type": "buildUploadFiles", "id": file_id,
                       "attributes": {"uploaded": True,
                                      "sourceFileChecksum": md5_of(ipa)}}}
    try:
        client.patch(f"/v1/buildUploadFiles/{file_id}", commit)
    except ApiError as e:
        if e.status not in (400, 409, 422) or "sourceFileChecksum" not in str(e):
            raise
        note("    note: this resource rejected sourceFileChecksum; committing "
             "without it. The upload is still size-checked by App Store Connect.")
        del commit["data"]["attributes"]["sourceFileChecksum"]
        client.patch(f"/v1/buildUploadFiles/{file_id}", commit)
    note("    committed")

    # Processing is asynchronous. Polling to a terminal state is what turns
    # "the bytes were accepted" into "Apple built a TestFlight build out of
    # them", which are different claims and only the second one is useful.
    deadline = time.time() + timeout_s
    last = None
    while time.time() < deadline:
        state = client.get(f"/v1/buildUploads/{upload_id}")["data"]["attributes"].get(
            "state", "UNKNOWN")
        if state != last:
            note(f"    state: {state}")
            last = state
        if state in ("COMPLETE", "SUCCEEDED"):
            note(f"==> accepted. buildUpload {upload_id}")
            return 0
        if state in ("FAILED", "INVALID", "CANCELLED"):
            detail = client.get(f"/v1/buildUploads/{upload_id}")
            fail(f"App Store Connect ended the upload in state {state}:\n"
                 f"{json.dumps(detail, indent=2)}")
        time.sleep(15)

    # Not a failure of this repository: the build is uploaded and Apple is
    # still working on it. Say which id to look at rather than reporting a
    # timeout as a rejection.
    note(f"==> uploaded, still processing after {timeout_s}s. "
         f"buildUpload {upload_id} -- check App Store Connect, or "
         f"GET /v1/buildUploads/{upload_id}.")
    return 0


# -------------------------------------------------------------- the rehearsal


def rehearse(ipa: str | None) -> int:
    """Everything the upload does that does not need Apple on the other end.

    The point is that a typo in the token construction is a red pull request
    rather than a red release. It cannot prove Apple would accept a real key --
    nothing off a Mac-free runner with no account can -- it proves this script
    builds a well-formed ES256 JWT from a key of the shape Apple hands out, and
    that the DER->JOSE conversion is right, checked with openssl's own verifier
    rather than by reading the code again.
    """
    with tempfile.TemporaryDirectory() as d:
        key = os.path.join(d, "AuthKey_REHEARSAL0.p8")
        pub = os.path.join(d, "pub.pem")
        # An App Store Connect key is a P-256 private key in PKCS#8 PEM, which
        # is exactly what this writes.
        subprocess.run(["openssl", "genpkey", "-algorithm", "EC", "-pkeyopt",
                        "ec_paramgen_curve:P-256", "-out", key],
                       check=True, capture_output=True)
        subprocess.run(["openssl", "pkey", "-in", key, "-pubout", "-out", pub],
                       check=True, capture_output=True)

        token = mint_token("00000000-0000-0000-0000-000000000000", "REHEARSAL0", key)
        parts = token.split(".")
        if len(parts) != 3:
            fail(f"the token has {len(parts)} dot-separated parts, not 3.")

        def unb64(s: str) -> bytes:
            return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))

        header, claims = json.loads(unb64(parts[0])), json.loads(unb64(parts[1]))
        for k, v in (("alg", "ES256"), ("typ", "JWT"), ("kid", "REHEARSAL0")):
            if header.get(k) != v:
                fail(f"token header {k} is {header.get(k)!r}, expected {v!r}.")
        if claims.get("aud") != AUDIENCE:
            fail(f"token aud is {claims.get('aud')!r}, expected {AUDIENCE!r}; "
                 "Apple rejects any other audience as unauthorized.")
        if not claims.get("iss"):
            fail("token carries no iss; a team key identifies itself by issuer id.")
        if not 0 < claims["exp"] - claims["iat"] <= 20 * 60:
            fail(f"token lifetime is {claims['exp'] - claims['iat']}s; Apple caps "
                 "App Store Connect tokens at 20 minutes.")

        # The real check: a signature this script produced, verified by
        # something that is not this script. Rebuild the DER from the raw r||s
        # and hand it to openssl.
        raw = unb64(parts[2])
        if len(raw) != 64:
            fail(f"the ES256 signature is {len(raw)} bytes; JOSE requires 64.")

        def der_int(b: bytes) -> bytes:
            b = b.lstrip(b"\x00") or b"\x00"
            if b[0] & 0x80:
                b = b"\x00" + b
            return b"\x02" + bytes([len(b)]) + b

        seq = der_int(raw[:32]) + der_int(raw[32:])
        der = b"\x30" + bytes([len(seq)]) + seq
        sig_path, msg_path = os.path.join(d, "sig"), os.path.join(d, "msg")
        with open(sig_path, "wb") as f:
            f.write(der)
        with open(msg_path, "wb") as f:
            f.write(f"{parts[0]}.{parts[1]}".encode())
        v = subprocess.run(["openssl", "dgst", "-sha256", "-verify", pub,
                            "-signature", sig_path, msg_path],
                           capture_output=True)
        if v.returncode != 0:
            fail("openssl could not verify the signature this script produced, so "
                 "the DER->JOSE conversion is wrong and every real token would be "
                 f"rejected: {v.stderr.decode(errors='replace').strip()}")
        note("ok: ES256 token signed, decoded and verified by openssl")

    if ipa:
        bundle_id, short_version, build_version = ipa_metadata(ipa)
        if not re.fullmatch(r"[0-9]+(\.[0-9]+)*", short_version):
            fail(f"CFBundleShortVersionString is {short_version!r}, which App Store "
                 "Connect will reject; it must be dot-separated numbers.")
        if not re.fullmatch(r"[0-9]+(\.[0-9]+)*", build_version):
            fail(f"CFBundleVersion is {build_version!r}, which App Store Connect "
                 "will reject; it must be dot-separated numbers.")
        note(f"ok: {os.path.basename(ipa)} declares {bundle_id} "
             f"{short_version} ({build_version})")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ipa")
    ap.add_argument("--platform", default="IOS")
    ap.add_argument("--timeout", type=int, default=1800,
                    help="seconds to wait for Apple to finish processing")
    ap.add_argument("--rehearse", action="store_true",
                    help="exercise the credential path with a throwaway key and "
                         "no network")
    a = ap.parse_args()

    if a.rehearse:
        return rehearse(a.ipa)

    if not a.ipa:
        fail("--ipa is required unless --rehearse is given.")
    if not os.path.isfile(a.ipa):
        fail(f"no such file: {a.ipa}")

    issuer = os.environ.get("APPSTORE_ISSUER_ID", "")
    key_id = os.environ.get("APPSTORE_KEY_ID", "")
    p8 = os.environ.get("APPSTORE_KEY_P8", "")
    missing = [n for n, v in (("APPSTORE_ISSUER_ID", issuer),
                              ("APPSTORE_KEY_ID", key_id),
                              ("APPSTORE_KEY_P8", p8)) if not v]
    if missing:
        fail(f"{', '.join(missing)} not set. All three are required to reach App "
             "Store Connect; see packaging/ios/Makefile for where they come from.")

    with tempfile.TemporaryDirectory() as d:
        key_path = os.path.join(d, "AuthKey.p8")
        # The secret holds either the .p8 text Apple hands out or a base64 of
        # it, because both are things a person reasonably pastes into a GitHub
        # secret and only one of them survives a copy that eats newlines.
        blob = p8.strip()
        if "-----BEGIN" not in blob:
            try:
                blob = base64.b64decode(blob, validate=True).decode()
            except Exception:
                fail("APPSTORE_KEY_P8 is neither PEM text nor valid base64 of it.")
        if "-----BEGIN" not in blob:
            fail("APPSTORE_KEY_P8 decoded to something with no PEM header.")
        with open(key_path, "w") as f:
            f.write(blob if blob.endswith("\n") else blob + "\n")
        os.chmod(key_path, 0o600)
        try:
            return upload(Client(issuer, key_id, key_path), a.ipa, a.platform,
                          a.timeout)
        except ApiError as e:
            # Apple's refusal, printed as a refusal. A traceback here would
            # bury the one part that says which attribute was wrong under
            # frames from this file, and 401 NOT_AUTHORIZED in particular has
            # a short list of causes worth naming.
            hint = ""
            if e.status == 401:
                hint = ("\nA 401 means the token was well formed and the key was "
                        "not accepted: check APPSTORE_KEY_ID against the key, "
                        "APPSTORE_ISSUER_ID against the team, and that the key "
                        "has not been revoked.")
            elif e.status == 403:
                hint = ("\nA 403 usually means the key's role is too low. "
                        "Uploading builds needs App Manager; Developer -- the "
                        "role notarization uses -- is not enough.")
            elif e.status == 409:
                hint = ("\nA 409 is usually a CFBundleVersion App Store Connect "
                        "has already seen. Bump CURRENT_PROJECT_VERSION in "
                        "packaging/ios/project.yml.")
            fail(f"{e}{hint}")
        except RuntimeError as e:
            fail(str(e))
    return 1


if __name__ == "__main__":
    sys.exit(main())
