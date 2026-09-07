> **Alpha Software** — Mununu is under active development. APIs, syntax, and behavior may change. We welcome feedback and bug reports via [GitHub Issues](https://github.com/Mumunu-team/mununu/issues).

# References

This page collects the academic and industrial references that inform Mununu's design and implementation. Each entry includes a brief note on its relevance to the tool.

---

## Table of Contents

- [Modal Mu-Calculus](#modal-mu-calculus)
- [Linear Temporal Logic (LTL)](#linear-temporal-logic-ltl)
- [GR(1) Synthesis](#gr1-synthesis)
- [Controller Synthesis and Supervisory Control](#controller-synthesis-and-supervisory-control)
- [Process Algebra and Labeled Transition Systems](#process-algebra-and-labeled-transition-systems)
- [Bisimulation Minimization](#bisimulation-minimization)
- [Game Theory and Parity Games](#game-theory-and-parity-games)
- [Abstraction](#abstraction)
- [Textbooks](#textbooks)
- [Comparable Tools](#comparable-tools)

---

## Modal Mu-Calculus

**Kozen, D. (1983).** "Results on the Propositional Mu-Calculus." _Theoretical Computer Science_, 27(3), 333--354.
Foundational paper defining the propositional mu-calculus with least and greatest fixpoint operators. Mununu's core formula language and fixpoint evaluation engine directly implement Kozen's semantics.

**Emerson, E. A. & Lei, C.-L. (1986).** "Efficient Model Checking in Fragments of the Propositional Mu-Calculus." _Proceedings of the First IEEE Symposium on Logic in Computer Science (LICS)_, 267--278.
Establishes complexity results and practical evaluation strategies for mu-calculus fragments. Mununu's bitvec-backed fixpoint iteration follows the efficient evaluation approach described here.

**Bradfield, J. & Stirling, C. (2007).** "Modal Mu-Calculi." _Handbook of Modal Logic_ (P. Blackburn, J. van Benthem, F. Wolter, eds.), Elsevier, 721--756.
Comprehensive survey of mu-calculus theory, including alternation depth, expressiveness, and connections to automata theory. Serves as the theoretical reference for Mununu's formula simplification and alternation analysis.

---

## Linear Temporal Logic (LTL)

**Pnueli, A. (1977).** "The Temporal Logic of Programs." _Proceedings of the 18th IEEE Symposium on Foundations of Computer Science (FOCS)_, 46--57.
Introduces temporal logic as a specification language for concurrent programs. Mununu's LTL pattern library translates common temporal properties (safety, liveness, response) into mu-calculus for evaluation.

**Vardi, M. Y. & Wolper, P. (1986).** "An Automata-Theoretic Approach to Automatic Program Verification." _Proceedings of the First IEEE Symposium on Logic in Computer Science (LICS)_, 332--344.
Establishes the automata-theoretic framework connecting LTL to Buchi automata. Informs Mununu's LTL-to-mu-calculus translation module, which converts temporal specifications into fixpoint formulas.

---

## GR(1) Synthesis

**Piterman, N., Pnueli, A. & Sa'ar, Y. (2006).** "Synthesis of Reactive(1) Designs." _Proceedings of the 7th International Conference on Verification, Model Checking, and Abstract Interpretation (VMCAI)_, Springer LNCS 3855, 364--380.
Defines the GR(1) fragment and its polynomial-time synthesis algorithm. Mununu implements GR(1) synthesis as a special case of its general controller synthesis framework, using the three-nested-fixpoint algorithm from this paper.

**Bloem, R., Jobstmann, B., Piterman, N., Pnueli, A. & Sa'ar, Y. (2012).** "Synthesis of Reactive(1) Designs." _Journal of Computer and System Sciences_, 78(3), 911--938.
Extended journal version of the VMCAI paper with correctness proofs, optimizations, and experimental evaluation. Mununu's GR(1) examples and test suite validate against the specifications and expected outcomes described here.

---

## Controller Synthesis and Supervisory Control

**Ramadge, P. J. & Wonham, W. M. (1987).** "Supervisory Control of a Class of Discrete Event Processes." _SIAM Journal on Control and Optimization_, 25(1), 206--230.
Foundational work on supervisory control theory for discrete event systems, introducing the distinction between controllable and uncontrollable events. Mununu's controller synthesis partitions transitions into controllable and uncontrollable sets following this framework.

**Ramadge, P. J. & Wonham, W. M. (1989).** "The Control of Discrete Event Systems." _Proceedings of the IEEE_, 77(1), 81--98.
Survey paper consolidating supervisory control theory. Provides the theoretical basis for Mununu's approach to restricting system behavior through controller synthesis while preserving uncontrollable environment transitions.

---

## Process Algebra and Labeled Transition Systems

**Milner, R. (1989).** _Communication and Concurrency._ Prentice Hall.
Defines the Calculus of Communicating Systems (CCS) and labeled transition systems as a semantic model. Mununu's CLTS data structure and synchronous/asynchronous composition operators are rooted in Milner's formalism.

**Hoare, C. A. R. (1985).** _Communicating Sequential Processes._ Prentice Hall.
Introduces CSP and its algebraic treatment of concurrency. Mununu's composition engine supports CSP-style synchronization on shared actions, and the CTXDSL language draws on CSP conventions for specifying parallel composition.

---

## Bisimulation Minimization

**Paige, R. & Tarjan, R. E. (1987).** "Three Partition Refinement Algorithms." _SIAM Journal on Computing_, 16(6), 973--989.
Presents efficient partition refinement algorithms for computing bisimulation equivalences. Mununu's controller minimization pass implements bisimulation quotienting based on this algorithm, reducing controller size while preserving behavioral equivalence.

---

## Game Theory and Parity Games

**Zielonka, W. (1998).** "Infinite Games on Finitely Coloured Graphs with Applications to Automata on Infinite Trees." _Theoretical Computer Science_, 200(1--2), 135--183.
Presents the recursive algorithm for solving parity games. Mununu uses game-theoretic reasoning internally when computing winning regions for controller synthesis and counterstrategy generation.

**Emerson, E. A. & Jutla, C. S. (1991).** "Tree Automata, Mu-Calculus and Determinacy." _Proceedings of the 32nd IEEE Symposium on Foundations of Computer Science (FOCS)_, 368--377.
Establishes the connection between mu-calculus model checking and parity games through tree automata. Provides theoretical justification for Mununu's approach of reducing synthesis problems to fixpoint computations.

**Gradel, E., Thomas, W. & Wilke, T. (eds.) (2002).** _Automata, Logics, and Infinite Games: A Guide to Current Research._ Springer LNCS 2500.
Comprehensive collection covering the interplay between automata, logic, and games. Serves as a general reference for the game-theoretic foundations underlying Mununu's synthesis and verification algorithms.

---

## Abstraction

**Cousot, P. & Cousot, R. (1977).** "Abstract Interpretation: A Unified Lattice Model for Static Analysis of Programs by Construction or Approximation of Fixpoints." _Proceedings of the 4th ACM SIGACT-SIGPLAN Symposium on Principles of Programming Languages (POPL)_, 238--252.
Foundational paper on abstract interpretation. Mununu's state variable abstraction module (Boolean, integer interval, and symbol-set domains) applies abstract interpretation principles to reduce infinite or large state spaces to finite, analyzable models.

**Clarke, E. M., Grumberg, O. & Long, D. E. (1994).** "Model Checking and Abstraction." _ACM Transactions on Programming Languages and Systems (TOPLAS)_, 16(5), 1512--1542.
Connects abstract interpretation with temporal logic model checking. Informs Mununu's multi-level abstraction strategy, which unrolls abstract state variables into concrete CLTS states while preserving the properties of interest.

---

## Textbooks

**Clarke, E. M., Grumberg, O. & Peled, D. A. (1999; 2nd ed. 2018).** _Model Checking._ MIT Press.
The standard textbook on model checking, covering CTL, LTL, mu-calculus, BDDs, partial order reduction, and abstraction. Serves as the primary pedagogical reference for Mununu's verification concepts and is recommended reading for users new to formal verification.

---

## Comparable Tools

The following table positions Mununu relative to established verification and synthesis tools.

| Tool | Focus | Relation to Mununu |
|------|-------|--------------------|
| [SPIN](https://spinroot.com/) | LTL model checking with Promela, explicit-state exploration | Mununu uses fixpoint-based mu-calculus evaluation rather than explicit-state search; supports synthesis in addition to verification. |
| [UPPAAL](https://uppaal.org/) | Timed automata model checking and synthesis | Mununu operates on untimed systems but supports the full mu-calculus, which is strictly more expressive than the timed-CTL fragment used by UPPAAL. |
| [TLA+](https://lamport.azurewebsites.net/tla/tla.html) | Specification of infinite-state concurrent systems | Mununu targets finite-state systems with automatic synthesis; TLA+ focuses on specification and proof rather than controller generation. |
| [NuSMV](https://nusmv.fbk.eu/) | BDD-based symbolic model checking for CTL and LTL | Mununu uses bitvec-backed sets instead of BDDs and adds controller synthesis; NuSMV is verification-only. |
| [Strix](https://strix.model.in.tum.de/) | LTL-to-DPA reactive synthesis | Mununu works with compositional labeled transition systems and mu-calculus directly rather than converting from LTL to deterministic parity automata. |
| [Acacia+](https://projects.lsv.fr/acacia/) | Antichain-based LTL synthesis | A complementary approach to reactive synthesis; Mununu's fixpoint evaluation provides a different algorithmic foundation. |
| [SLUGS](https://github.com/VerifiableRobotics/slugs) | BDD-based GR(1) synthesis | Mununu supports GR(1) as a special case within a broader framework that includes full mu-calculus and multi-mode composition. |
