// Copyright 2015-2025 Deyan Ginev. MIT license. Vanilla WebAuthn helpers for the CorTeX admin UI
// (docs/archive/WEBAUTHN_DESIGN.md) — no framework. Converts the base64url JSON the server speaks to/from the
// ArrayBuffers the browser's navigator.credentials API requires, and drives the enrollment ceremony.

(function () {
  "use strict";

  function b64urlToBuf(value) {
    var s = value.replace(/-/g, "+").replace(/_/g, "/");
    var pad = s.length % 4;
    if (pad) { s += "====".slice(pad); }
    var bin = atob(s);
    var buf = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) { buf[i] = bin.charCodeAt(i); }
    return buf.buffer;
  }

  function bufToB64url(buf) {
    var bytes = new Uint8Array(buf);
    var s = "";
    for (var i = 0; i < bytes.length; i++) { s += String.fromCharCode(bytes[i]); }
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  // Enroll a new passkey for the signed-in admin. `label` is a human name for the authenticator.
  async function enrollPasskey(label) {
    if (!window.PublicKeyCredential) {
      alert("This browser does not support passkeys (WebAuthn).");
      return;
    }
    var begin = await fetch("/admin/passkeys/register/begin", { method: "POST" });
    if (begin.status === 401) { window.location = "/admin/login"; return; }
    if (begin.status === 503) { alert("Passkey sign-in is not enabled on this deployment."); return; }
    if (!begin.ok) { alert("Could not start passkey enrollment."); return; }

    var options = (await begin.json()).publicKey;
    options.challenge = b64urlToBuf(options.challenge);
    options.user.id = b64urlToBuf(options.user.id);
    if (options.excludeCredentials) {
      options.excludeCredentials.forEach(function (c) { c.id = b64urlToBuf(c.id); });
    }

    var credential;
    try {
      credential = await navigator.credentials.create({ publicKey: options });
    } catch (e) {
      alert("Passkey enrollment was cancelled or failed: " + e);
      return;
    }

    var body = {
      id: credential.id,
      rawId: bufToB64url(credential.rawId),
      type: credential.type,
      response: {
        attestationObject: bufToB64url(credential.response.attestationObject),
        clientDataJSON: bufToB64url(credential.response.clientDataJSON)
      }
    };
    var url = "/admin/passkeys/register/finish?label=" + encodeURIComponent(label || "passkey");
    var finish = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body)
    });
    if (finish.ok) { window.location.reload(); }
    else { alert("Passkey enrollment failed to verify (status " + finish.status + ")."); }
  }

  // Sign in with a passkey for `owner`. On success the server set the session cookie; go to `next`.
  async function signInWithPasskey(owner, next) {
    var err = document.getElementById("passkey-error");
    function fail(msg) { if (err) { err.textContent = msg; } else { alert(msg); } }
    if (err) { err.textContent = ""; }
    if (!window.PublicKeyCredential) { fail("This browser does not support passkeys."); return; }
    if (!owner) { fail("Enter your name to sign in with a passkey."); return; }

    var begin = await fetch("/admin/passkeys/auth/begin?owner=" + encodeURIComponent(owner), { method: "POST" });
    if (begin.status === 404) { fail("No passkeys are enrolled for that name."); return; }
    if (begin.status === 503) { fail("Passkey sign-in is not enabled on this deployment."); return; }
    if (!begin.ok) { fail("Could not start passkey sign-in."); return; }

    var options = (await begin.json()).publicKey;
    options.challenge = b64urlToBuf(options.challenge);
    if (options.allowCredentials) {
      options.allowCredentials.forEach(function (c) { c.id = b64urlToBuf(c.id); });
    }

    var assertion;
    try {
      assertion = await navigator.credentials.get({ publicKey: options });
    } catch (e) {
      fail("Passkey sign-in was cancelled or failed.");
      return;
    }

    var body = {
      id: assertion.id,
      rawId: bufToB64url(assertion.rawId),
      type: assertion.type,
      response: {
        authenticatorData: bufToB64url(assertion.response.authenticatorData),
        clientDataJSON: bufToB64url(assertion.response.clientDataJSON),
        signature: bufToB64url(assertion.response.signature),
        userHandle: assertion.response.userHandle ? bufToB64url(assertion.response.userHandle) : null
      }
    };
    var finish = await fetch("/admin/passkeys/auth/finish", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body)
    });
    if (finish.ok) { window.location = next || "/admin"; }
    else { fail("Passkey sign-in failed to verify (status " + finish.status + ")."); }
  }

  // Wire the management-page "Enroll a passkey" button + the login-page "Sign in with a passkey".
  document.addEventListener("DOMContentLoaded", function () {
    var enroll = document.getElementById("enroll-passkey");
    if (enroll) {
      enroll.addEventListener("click", function () {
        var label = prompt("Name this authenticator (e.g. 'Laptop Touch ID', 'YubiKey'):", "passkey");
        if (label !== null) { enrollPasskey(label); }
      });
    }
    var signin = document.getElementById("signin-passkey");
    if (signin) {
      signin.addEventListener("click", function () {
        var owner = document.getElementById("passkey-owner");
        signInWithPasskey(owner ? owner.value.trim() : "", signin.getAttribute("data-next") || "/admin");
      });
    }
  });

  window.cortexWebauthn = { enrollPasskey: enrollPasskey, signInWithPasskey: signInWithPasskey };
})();
