# Mununu Commercial Licence — DRAFT

> Status: **draft, pending counsel — not in force.** Nothing in this file grants any right until
> it is reviewed by counsel and this banner is removed in a signed-off release. Until then the
> only terms that apply are those in [`LICENSE`](LICENSE) (non-commercial use) and any separate
> written agreement signed by the copyright holder.

This is the "separate written licence" that the Commercial Use section of [`LICENSE`](LICENSE)
refers to. It applies to a Licensee who holds a **valid licence file** (`mununu.lic`) issued for
it by the copyright holder, together with an order form or invoice that refers to that file.

## 1. Definitions

- **Software** — the first-party work in this repository and the binaries built from it, in the
  version the Licensee runs. Third-party components listed in [`THIRD_PARTY.md`](THIRD_PARTY.md)
  are excluded; they remain under their own licences (§6).
- **Licence file** — a `mununu.lic` file whose signature verifies against an issuer key compiled
  into the Software. Its claims name the **Licensee** (`licensee`), the **Team** (`team_id`), the
  **Tier** (`tier`), and the **Term** (from `issued` to `expires`, both inclusive, followed by
  `grace_days` days of grace). `mununu licence status` shows these values (format:
  [`docs/licensing.md`](docs/licensing.md)).
- **Order form** — the quote, invoice or agreement under which the Licence file was issued. It
  sets the fee and any limits on the number of users or machines.

## 2. Grant

While the Licence file is valid or in its grace days, and subject to this licence and the Order
form, the copyright holder grants the Licensee a non-exclusive, non-transferable,
non-sublicensable licence to:

1. **Install and run** the Software on machines the Licensee controls, including its own cloud
   accounts, CI runners and air-gapped sites. Running "on behalf of" the Licensee by its
   contractors on its behalf is included.
2. **Use it commercially.** This includes verifying designs, models and software that the
   Licensee sells or uses for commercial gain, and running it in the CI of a paid product.
3. **Integrate it into the Licensee's internal tools and pipelines**, including through the
   HTTP API, the MCP server and the command line.
4. **Modify it for internal use.** The Licensee may change the Software for its own use. It may
   not distribute modified versions outside its organisation.

The **evaluation** Tier grants items 1–3 for evaluation only, for the Term stated in its Licence
file. Absent a Licence file, the Software runs for 30 days from its first run on a host for
evaluation (the built-in term, [`docs/licensing.md`](docs/licensing.md)).

## 3. Restrictions

The Licensee may not:

- redistribute the Software, or offer it to third parties as a hosted service, except as an
  Order form allows;
- remove or alter copyright notices, `LICENSE`, `THIRD_PARTY.md` or `LICENSES/`;
- share its Licence file outside the Team named in it, or use it after the Term and grace days;
- work around the licence check, or forge or alter a Licence file.

## 4. Verdicts and the licence check

The Software reports the licence state and never uses it to change a verdict: a run's results
are the same in every licence state (`crates/mununu-core/tests/licence_never_reaches_a_verdict_h1.rs`
keeps that true). Past its grace days, the Software stops running. Results produced while the
Licence file was valid stay the Licensee's to keep and use.

## 5. Term and termination

This licence lasts for the Term in the Licence file and ends on its own after the grace days. It
ends earlier if the Licensee breaches §3 and does not cure the breach within 30 days of notice.
On termination the Licensee stops running the Software; §§4, 6, 7 and 8 survive.

## 6. Third-party components

The Software links or ships third-party components (for example Z3, MIT). Each is licensed to
the Licensee by its own authors under the licence named in [`THIRD_PARTY.md`](THIRD_PARTY.md) and
`LICENSES/`; this licence grants nothing in them and takes nothing away. Solver binaries that
the Software runs as separate processes (Yosys, slang, Verilator and others) are not part of the
Software.

## 7. Warranty and liability

Unless an Order form says otherwise, the Software is provided as in the Disclaimer of
[`LICENSE`](LICENSE), with no warranty. A formal-verification verdict is a statement about the
model the Software built from the inputs given to it. Its transfer to the real system depends on
the abstraction and the property (see [`docs/abstraction.md`](docs/abstraction.md)). The copyright
holder's total liability under this licence is limited to the fees paid for the Term.

## 8. General

Governing law and venue: **[to be set by counsel]**. This licence, the Licence file and the Order
form are the whole agreement on their subject. If they conflict, the Order form prevails, then
this licence. A Licensee's own purchase terms do not apply unless the copyright holder signs
them.

---

## Open questions for counsel (remove before release)

1. **Seats and machines.** The Licence file carries a Team, not a seat count. Should limits live
   only in the Order form (as drafted), or be added to the file's claims?
2. **Contractors and affiliates.** §2.1 covers contractors working for the Licensee. Should
   affiliates (parent and subsidiary companies) be included?
3. **Hosting for customers.** Should a services firm running Mununu for its own clients' designs
   need a separate tier? As drafted, the Order form must allow it (§3).
4. **Liability cap and governing law.** §7's cap and §8's law are placeholders.
5. **Interaction with a future AGPLv3 relicensing.** This draft assumes `LICENSE` stays
   non-commercial. If the public licence becomes AGPLv3, this text becomes the
   proprietary-licence alternative, and §2.4 and §3's redistribution rules need review.
6. **G1's third-party notices.** Should they be reviewed in the same pass? (`THIRD_PARTY.md`,
   `LICENSES/`, and the release archives' `LICENSING.md`.)

The roadmap's falsifier for this text: a customer's lawyer can answer "may we run this on our CI
for a paid product?" from `LICENSE` + this file + their `mununu.lic`, and nothing else. As
drafted, the answer is in §2.2.
