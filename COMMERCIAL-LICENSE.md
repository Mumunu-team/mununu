# Mununu Commercial Licence — DRAFT

> Status: **draft, pending counsel — not in force.** Nothing in this file grants any right until
> it is reviewed by counsel and this banner is removed in a signed-off release. Until then the
> only terms that apply are those in [`LICENSE`](LICENSE) (non-commercial use) and any separate
> written agreement signed by the Licensor. Text in **[brackets]** is a placeholder for counsel.

This is the "separate written licence" that the Commercial Use section of [`LICENSE`](LICENSE)
refers to. It has two parts:

- **Part A, Evaluation**, applies to any organisation that runs the Software to decide whether
  to buy it, with or without a Licence file, including for a commercial purpose.
- **Part B, Commercial use**, applies to a Licensee that holds a **valid Licence file** together
  with an **Order form** that refers to it. A Licence file on its own grants no commercial right.

Non-commercial use needs neither part: [`LICENSE`](LICENSE) already permits it, with no term.

## 1. Definitions

- **Licensor** — the holder of the copyright in the Software: today **Mariano Cerrutti**;
  **[after incorporation and assignment, the company that holds the copyright]**.
- **Software** — the first-party work in this repository and the binaries and images built from
  it, in the version the Licensee runs. Third-party components listed in
  [`THIRD_PARTY.md`](THIRD_PARTY.md) are excluded; they remain under their own licences (§8).
- **Licence file** — a `mununu.lic` file whose signature verifies against an issuer key compiled
  into the Software. Its claims name the **Licensee** (`licensee`), the **Team** (`team_id`), the
  **Tier** (`tier`), and the **Term** (from `issued` to `expires`, both inclusive, followed by
  `grace_days` days of grace). `mununu licence status` shows these values (format:
  [`docs/licensing.md`](docs/licensing.md)).
- **Order form** — the quote, invoice or agreement under which the Licence file was issued for a
  fee. It sets the fee and any limits on users, machines or use.
- **Client work** — verification the Licensee performs for its own customers, on their designs,
  as part of services the Licensee delivers.

## Part A — Evaluation

### 2. Evaluation grant

The Licensor grants any organisation a non-exclusive, non-transferable, free licence to install
and run the Software **to evaluate it**, including on designs it develops commercially, for:

- **30 days** from the Software's first run on a host when there is no Licence file (the built-in
  term, [`docs/licensing.md`](docs/licensing.md)), or
- the Term of an evaluation-Tier Licence file, when one has been issued.

Evaluation does not include using the Software's results in a product's release or sign-off,
running it in a production CI, or offering it to others. When the term ends, evaluation ends;
results produced during it stay the evaluator's.

## Part B — Commercial use

### 3. Grant

While the Licence file is valid or in its grace days, and subject to this licence and the Order
form, the Licensor grants the Licensee a non-exclusive, non-transferable, non-sublicensable
licence to:

1. **Install and run** the Software on machines the Licensee controls, including its own cloud
   accounts, CI runners and air-gapped sites. Running it for the Licensee by the Licensee's
   contractors is included.
2. **Use it commercially.** This includes verifying designs, models and software that the
   Licensee sells or uses for commercial gain, and running it in the CI of a paid product.
3. **Integrate it into the Licensee's internal tools and pipelines**, including through the
   HTTP API, the MCP server and the command line.
4. **Modify it for internal use.** The Licensee may change the Software for its own use. It may
   not distribute modified versions outside its organisation.
5. **Client work.** The Licensee may use the Software for Client work without a fee per client,
   and may hand its clients the results (reports, counterexamples, certificates). The clients get
   no right to run the Software themselves.

A Licence file issued **free of charge** (for example to an academic, individual or open-source
user) grants nothing beyond [`LICENSE`](LICENSE): it removes the evaluation stop and nothing else.
Commercial rights come only from an Order form.

### 4. Restrictions

The Licensee may not:

- redistribute the Software, or offer it to third parties as a hosted service they operate
  themselves, except as an Order form allows;
- remove or alter copyright notices, `LICENSE`, `THIRD_PARTY.md`, `LICENSES/` or the notices in
  the bundle image (`deploy/NOTICE.md`);
- share its Licence file outside the Team named in it, or use it after the Term and grace days;
- work around the licence check, or forge or alter a Licence file;
- use the Software **[in the design or verification of weapons-targeting systems or of systems
  for mass surveillance — scope and wording for counsel]**.

### 5. Audit

**[Placeholder for counsel.]** On reasonable written notice, no more than once a year, the
Licensee confirms in writing the number of users and machines running the Software. Where the
Licensee has enabled the opt-in local usage ledger (`[licensing] ledger`, see
[`docs/licensing.md`](docs/licensing.md)), it may provide the ledger as that confirmation. The
Software sends nothing to the Licensor on its own.

### 6. Verdicts and the licence check

The Software reports the licence state and never uses it to change a verdict: a run's results
are the same in every licence state (`crates/mununu-core/tests/licence_never_reaches_a_verdict_h1.rs`
keeps that true). Past its grace days, the Software stops running. Results produced while the
Licence file was valid stay the Licensee's to keep and use.

### 7. Term and termination

This licence lasts for the Term in the Licence file and ends on its own after the grace days. It
ends earlier:

- if the Licensee breaches §4 and does not cure the breach within 30 days of notice;
- if a fee under the Order form is unpaid **[N]** days after notice;
- **[on the Licensee's insolvency, or if the Licensee brings an intellectual-property claim
  against the Licensor over the Software — for counsel]**.

On termination the Licensee stops running the Software; §§6, 8, 9, 10 and 11 survive.

## Both parts

### 8. Third-party components

The Software links or ships third-party components (for example Z3, MIT), and the on-prem bundle
image also carries an unmodified oss-cad-suite release, a Debian base system and the web UI's npm
packages. Each is licensed by its own authors under the licence named in
[`THIRD_PARTY.md`](THIRD_PARTY.md), `LICENSES/` and the bundle's `NOTICE.md`; this licence grants
nothing in them and takes nothing away. The suite's tools run as separate processes and are not
part of the Software, but the Licensor's distribution of them follows their licences.

### 9. Warranty and liability

Unless an Order form says otherwise, the Software is provided as in the Disclaimer of
[`LICENSE`](LICENSE), with no warranty.

**A verdict is not a certification.** A formal-verification verdict is a statement about the
model the Software built from the inputs given to it. Its transfer to the real system depends on
the abstraction and the property (see [`docs/abstraction.md`](docs/abstraction.md)). The Software
does not certify a design as fit for any purpose, safe, or compliant with any standard.

The Licensor is not liable for indirect, incidental, special or consequential damages, or for
lost profits, data or business. Its total liability under this licence is limited to **[the
greater of the fees paid in the 12 months before the claim and a fixed amount; carve-outs for
counsel]**.

### 10. Export control and sanctions

**[Placeholder for counsel.]** The Licensee complies with the export-control and sanctions laws
that apply to it and to the Software, and does not export, re-export or provide the Software to a
person, country or end use those laws prohibit.

### 11. General

Governing law and venue: **[to be set by counsel]**. This licence, the Licence file and the Order
form are the whole agreement on their subject. If they conflict, the Order form prevails, then
this licence. A Licensee's own purchase terms do not apply unless the Licensor signs them.

---

## Open questions for counsel (remove before release)

1. **Licensor.** Who should the Licensor be before and after incorporation, and in what order
   should the IP assignment and any relicensing happen?
2. **Governing law and venue**, given a founder resident in Argentina, a planned US entity and
   customers in the EU and the US.
3. **Liability.** Is a cap tied to fees enforceable when the fee is zero (Part A)? What carve-outs
   belong in §9?
4. **Copyleft in the bundle.** The on-prem image carries an unmodified oss-cad-suite release with
   GPL and LGPL components (`THIRD_PARTY.md` § "Bundled in the on-prem image"). What does
   distributing it require — a written source offer, the sources beside the image, or pruning the
   suite to the components the Software runs?
5. **Export classification and sanctions** for the binary and the bundle (§10).
6. **AI-assisted authorship.** Much of the code was written with an AI assistant. How strong is
   the copyright both licences rest on?
7. **Contributions.** Is the draft [`CLA.md`](CLA.md) the right instrument, or a DCO? Is AGPLv3
   plus this licence workable while §4 restricts fields of use, which AGPL forbids in the public
   licence?
8. **Evaluation as its own instrument.** Should Part A be a separate short licence?
9. **`LICENSE` and the built-in stop.** `LICENSE` permits non-commercial use with no term, while
   the binary stops after 30 days without a Licence file, and `LICENSE` allows a non-commercial
   user to modify the check out. Should `LICENSE` say so, and should free Licence files carry a
   tier of their own (today a free file is a `team`-tier file, distinguished only by having no
   Order form)?
10. **Seats and machines.** The Licence file carries a Team, not a seat count. Should limits live
    only in the Order form (as drafted), or be added to the file's claims?
11. **Contractors and affiliates.** §3.1 covers contractors working for the Licensee. Should
    affiliates (parent and subsidiary companies) be included?

The roadmap's falsifier for this text: a customer's lawyer can answer "may we run this on our CI
for a paid product?" from `LICENSE` + this file + their `mununu.lic`, and nothing else. As
drafted, the answer is in §3.2. An evaluator's lawyer can answer "may we trial it on our product
design?" from Part A.
