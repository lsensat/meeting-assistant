/**
 * The open-source notices window.
 *
 * whisper.cpp and ggml are compiled from source and linked statically into the
 * binary, as is every Rust crate, so the app redistributes a good deal of MIT
 * and Apache-2.0 code. Those licences ask for their notice to accompany copies,
 * and a file in the git repository reaches nobody running an installer.
 */

// Installed before anything that can throw, so a failure below is reported on
// screen rather than leaving a blank window.
import "./errors.js";
import * as api from "./api.js";

const target = document.getElementById("licenses-text");

// `textContent`, not markup: this is a licence file and must render as the
// exact characters it contains.
api
  .thirdPartyLicenses()
  .then((text) => {
    target.textContent = text;
  })
  .catch((error) => {
    target.textContent = String(error);
  });
