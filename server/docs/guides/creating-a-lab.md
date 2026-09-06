# Creating a laboratory for your corpus application

Your laboratory should help you choose shapes and runbooks because they work
on your documents, for your users, under your operating constraints. The
deliverable is a tested set of configuration documents and the evidence for
choosing them. A polished demonstration is useful, but it is not that evidence.

This tutorial explains how to build that laboratory without prescribing a
programming language, interface or experiment framework. Start with a small
collection of documents, a question sheet and a way to retain Server requests
and responses. Add automation as repetition makes it worthwhile. You do not
need an existing laboratory implementation to follow the method.

The running example is an equipment support assistant whose corpus contains
manuals, approved service bulletins and historical support tickets. It must
find the applicable procedure, distinguish product revisions, cite evidence
and acknowledge when the available records cannot answer a question. Substitute
your own users, document families and decisions as you work through the steps.

## 1. Define what a good application must do

Begin with the decisions your users need to make. Interview people who know
the work, read representative requests, and list the consequences of a wrong
answer. “Answer questions about our documents” is too broad to guide a shape
or runbook decision.

For the support assistant, finding one troubleshooting instruction is a
different task from comparing two product revisions or explaining why an old
ticket recommends a procedure that a bulletin withdrew. Each task needs a
different evidence set and a different definition of success.

Write an application brief that establishes:

- The users, their permissions and the decisions the application supports.
- The question families it must handle, including follow-up conversations.
- The scope of permitted evidence and any date or product-version restrictions.
- What a complete answer contains: required facts, qualifications, citations
  and a usable presentation.
- When the application should ask for clarification, report conflicting
  evidence, give a partial answer or decline to answer.
- The acceptable response time, operating cost and consequences of failure.

Make source authority explicit. A newer support ticket does not automatically
overrule an approved manual. A bulletin might govern only one product revision.
Have a domain expert settle those rules before expecting a model to apply them.

**Keep:** an application brief and an initial list of question families. These
will determine what you measure and prevent the experiment from drifting toward
whatever happens to produce an impressive answer.

## 2. Build the smallest useful laboratory

Separate the environment that runs the application from the machinery that
evaluates it. Munarium Server is the system under test. Your laboratory prepares
cases, submits requests, retains observations and compares them with expectations.

| Part of the laboratory | Its responsibility |
|---|---|
| Isolated Server deployment | Run the actual shapes, runbooks, retrieval and application operations being evaluated. |
| Corpus inventory | Record the documents, their identities, revisions and extraction state. |
| Configuration history | Retain baseline and candidate shapes, runbooks and relevant deployment settings. |
| Case library and answer keys | Define questions, caller context, expected evidence and acceptable outcomes. |
| Experiment runner | Execute cases consistently, control sessions and limits, and record failures as well as successes. |
| Evaluation and review | Apply objective checks, support expert review and explain disagreements. |
| Results archive | Preserve the inputs, outputs, costs and decisions needed to reproduce a comparison. |

These are responsibilities, not necessarily separate applications. A spreadsheet
can hold the first answer key. A simple report can expose cases and their evidence.
A database becomes useful when you need to search thousands of observations or
compare many configurations. Build a console when it makes reviewing experiments
easier; do not make a console a prerequisite for starting.

Use a separate database or an otherwise deliberately isolated test environment.
Avoid experiments that can replace production runbooks, change live indexes or
consume another application's provider allowance. Keep credentials outside the
configuration documents and give each experiment a defined spending limit.

Start from a pinned Server release. The
[Docker Hub deployment tutorial](dev-guide.md#deploy-the-published-docker-hub-image)
provides a persistent local setup. Verify that the Server is ready and can
perform an authenticated write and read before investigating retrieval quality.
An environment that cannot preserve state is not a reliable measurement rig.

If your laboratory has its own retrieval or memory simulator, use it for
exploration and to test the laboratory itself. Qualify the final documents
through the actual Munarium Server and, eventually, the application's request
path. A result from a different engine does not establish Server behavior.

**Keep:** a description of the test environment, a recorded Server image digest,
and a repeatable way to create an isolated experiment.

## 3. Understand the corpus before choosing a shape

Inventory document families rather than treating the corpus as one pile of
files. Record how many documents belong to each family, their typical and
largest sizes, formats, languages, revisions and access restrictions. Identify
duplicates, missing attachments, scanned pages and documents that refer to
other documents.

Read a small but varied sample yourself. Include ordinary documents and the
awkward ones: a long table, a short amendment, a poor scan, repeated boilerplate,
a superseded instruction and two records with similar names. Look at the text
the Server actually extracted, not only the original PDF in a viewer.

For the support assistant, ask whether a procedure's heading stays with its
steps, whether model identifiers survive extraction, and whether a table still
connects a symptom to the correct remedy. If those relationships are already
missing from extracted text, retrieval tuning cannot recover them reliably.

Preserve stable logical filenames and record content hashes. In Munarium,
filenames participate in source identity and collection bindings, so a renamed
file can change more than its display label. Use the
[corpus-loading guide](loading-corpora.md) to understand those mechanics.

Create three useful scales: a tiny diagnostic collection you can inspect
completely, a representative pilot, and the full intended corpus. The diagnostic
collection explains failures. The pilot makes comparisons affordable. The full
corpus reveals competition, coverage gaps and operating costs that small samples
hide. Preserve whole evidence relationships when sampling; an amendment without
its base document is a different test problem.

**Keep:** a corpus manifest, representative samples, and an extraction-quality
review that identifies defects to fix before retrieval experiments.

## 4. Write the answer key independently of model output

Have a knowledgeable reviewer answer the initial questions directly from the
documents. For each case, record the required facts and the passages that
support them. The key should describe acceptable meaning, evidence and scope,
not demand that the system imitate one preferred paragraph.

Record the caller's permissions and the relevant time or product version with
the question. A case can be answerable for one caller and unanswerable for
another. A question about the current procedure has a different key from a
question about the procedure in force when an old ticket was opened.

| Question family | What its key needs |
|---|---|
| Direct lookup | The required fact, its qualifiers and an acceptable supporting passage. |
| Comparison | Evidence for each side and the dimensions that must be compared. |
| Conflict or supersession | The competing assertions, their sources and the rule for resolving or preserving the disagreement. |
| Enumeration or aggregation | The complete expected set, inclusion boundaries and any calculation assumptions. |
| Unanswerable question | Why the permitted corpus cannot establish the answer and what the response must avoid inventing. |
| Access-restricted question | The permitted evidence and the information that must not be revealed. |
| Conversation | The starting state, the sequence of turns and the evidence or decisions that each turn may carry forward. |

Keep answer keys, grading notes and synthetic expected answers outside the
retrieval corpus. Check collection bindings to ensure they cannot be ingested
accidentally. A system that retrieves the answer sheet can look excellent while
teaching you nothing about your shapes or runbooks.

Distinguish several kinds of missing answer. “No such record exists in the
defined collection” requires adequate coverage to establish absence. “The search
did not find it” is a limitation of the search. “The caller cannot access it”
is an authorization boundary. Grade those outcomes differently.

Use synthetic documents to create precise diagnostic cases, such as two
procedures that differ only in product applicability. Label them as synthetic
and keep their results separate from representative real-corpus measurements.
Ask a second reviewer to adjudicate disputed keys. Version corrections to the
key and rerun affected comparisons; do not silently change expectations to make
a candidate pass.

**Keep:** a reviewed case library with evidence references, acceptable variants,
forbidden assertions and the reason each case exists.

## 5. Reserve questions you will not tune against

Divide cases into a development set, a validation set used at planned decision
points, and a held-out acceptance set. Keep close paraphrases and related
document families together when splitting. Otherwise a nearly identical question
can appear in both development and acceptance and exaggerate generalization.

The split concerns the task you are evaluating. For retrieval over an existing
corpus, acceptance questions can refer to documents that are already indexed;
their questions and answer keys remain unavailable during tuning. To test
generalization to new documents, reserve document families or later corpus
snapshots as well. State which of those claims your evaluation supports.

Once you repeatedly inspect and tune against an acceptance failure, that case
has become a development case. Keep it as a regression test and replenish the
held-out set. Do not rename a well-practiced battery “unseen” at release time.

**Keep:** a written split policy and a record of when each set was exposed to
the people or agents doing the tuning.

## 6. Establish a baseline and test your testing machinery

Begin with a simple, valid shape for each materially different document family
and a runbook that expresses the application's evidence and answer policy.
Use public examples as references for document structure, not as evidence that
their retrieval settings suit your corpus.

Validate the documents and their references before measuring answers. Confirm
that collections include the intended sources, that shape versions resolve,
and that the indexes used by a session were built from those versions. Retain
the applied documents as well as the files you intended to apply.

Test the laboratory with deliberately good and bad saved responses before
paying for generation. A missing citation, an incorrect number, an unsupported
claim and a truncated response should produce the intended verdicts. A correct
paraphrase should not fail merely because it uses different wording. These
fixtures test your evaluator; they are not evidence of live model quality.

Run the baseline on a small diagnostic battery, then on the pilot. Read the
complete answers and supporting evidence. Do not begin by sweeping dozens of
settings: first establish which failures you can explain.

For independent questions, create fresh sessions so earlier answers do not
help later cases. For conversational cases, replay the same starting state
and turn sequence. Keep those result categories separate. Record whether
caches are cold or warm and whether index construction is included in timing.

**Keep:** a reproducible baseline, a tested evaluator, and a short list of
observed failure mechanisms.

## 7. Improve shapes by inspecting what they preserve

A shape is a versioned description of how a class of sources is represented
and indexed, with evidence semantics where applicable. Its quality depends on
whether useful distinctions survive into the evidence the application receives.

Start with boundaries. Examine chunks around headings, numbered procedures,
tables and qualifications. Small chunks can separate a claim from the condition
that makes it true. Large chunks can carry the needed context while crowding
other evidence out of an answer. Choose candidates based on observed document
structure, then measure both retrieval and final-answer effects.

For the support assistant, inspect whether an instruction retains the product
revision to which it applies. A retrieved step without that qualifier can lead
to a fluent but incorrect answer. A chunk containing several unrelated product
procedures can create a different ambiguity. The goal is usable evidence, not
one universal chunk size.

Use separate shapes when document families have materially different structure
or evidence meaning. Avoid creating a shape for every filename without a reason.
Where you use evidence authority declarations, have a domain reviewer justify
them; a summary or historical ticket should not acquire controlling authority
merely because it is convenient to retrieve.

Treat any shape change that affects extraction, chunking or indexing as a
candidate requiring a corresponding index build. Compare fresh candidate
artifacts with the baseline; do not attribute a result to a shape change while
the session still reads the old index. Retain the previous shape and index
identities so a baseline remains reproducible.

**Keep:** a small set of shape candidates, inspected chunk examples, and measured
evidence about which boundaries and document distinctions each candidate preserves.

## 8. Improve runbooks in the order evidence reaches the answer

A runbook coordinates how the application uses its collections, retrieval,
evidence rules and completion policy. Diagnose the path in order instead of
changing the final instruction whenever an answer disappoints you.

First confirm collection coverage and access. Then examine candidate retrieval,
ranking and the final evidence supplied for completion. Only after those steps
should you tune how the model explains the evidence.

| Observation | Investigate before changing the prompt |
|---|---|
| The necessary document is absent from the index | Ingestion, extraction, collection binding or index build state. |
| It is indexed but not retrieved | Query vocabulary, retrieval method, candidate depth and caller access. |
| It is retrieved but displaced by repetitive material | Ranking, duplicate content and competition between collections. |
| It appears in reported hits but not in the completion evidence | Context limits and the construction of the actual completion request. |
| Both sides of a conflict are available but one disappears from the answer | Evidence policy, synthesis instructions, output limits and model behavior. |
| The answer sounds complete but omits members of a required set | Whether retrieval can establish the full set and whether synthesis preserves it. |

Change evidence quantity and context capacity together when they are coupled.
Requesting more hits cannot help if the extra material never fits in the
completion context. Conversely, a larger context can add distractors and cost
without improving coverage. Measure the evidence that reaches the model, not
just the number of reported hits. If your instrumentation cannot show that
boundary, record the uncertainty rather than claiming it has been verified.

Budget the answer as well as its input. A comparison may need room for both
sides, qualifications and citations. An incomplete response or a retry is an
operational outcome to measure, not a successful concise answer. The
[retrieval-sizing guide](retrieval-sizing.md) and
[token-budget reference](../tokenbudgets.md) explain the Server controls;
use your corpus measurements to choose settings rather than copying a fixed
recipe for every application.

Define how to cite, what to do with conflicting records, and when to disclose
insufficient evidence. Specify the useful answer structure without teaching
the runbook the answers to the test questions. Repeatedly adding benchmark
names, numbers or distinctive question phrases to instructions is a warning
that the configuration is learning the test rather than the task.

Recognize tasks that a ranked sample cannot establish. “List every applicable
bulletin” needs evidence of completeness; a larger search result is not that
proof. Consider a workflow that explicitly covers the relevant set, or a
separately validated structured-evidence path where appropriate. If the system
cannot establish completeness, narrow the promise made to the user and grade
the stated limitation honestly.

**Keep:** a runbook candidate with a stated reason for each significant change
and an observed explanation of how it affects the evidence-to-answer path.

## 9. Make each experiment answer a specific question

Write the hypothesis before starting a run. For example: preserving the
procedure heading with its steps will reduce wrong-revision answers without
increasing unsupported claims. Name the cases that should improve and the
cases that might regress.

Change one coherent factor at a time while diagnosing. Keep the Server build,
corpus snapshot, caller context, model configuration and grading rules fixed.
When two settings necessarily interact, compare their combinations explicitly
and explain the interaction. Do not describe a simultaneous model, shape and
runbook change as evidence about any one of them.

Compare baseline and candidate on the same cases. Alternate or randomize their
execution order when provider load or caching could bias timing. Repeat runs
under a declared policy and keep every attempt, including failures and retries.
Continue measuring promising candidates across the model tiers the application
will actually offer. Success on an expensive tier does not establish the
quality of a cheaper fallback.

Set the experiment's spending and time limits in advance. Include indexing,
embeddings, OCR, query rewriting, generation, retries and model-based judging
where your configuration uses them. A stopped run is partial coverage. Preserve
its results and costs, and decide explicitly whether another run is worthwhile.

**Keep:** an experiment record containing the hypothesis, controlled variables,
candidate change, complete run identities and the decision it supports.

## 10. Score evidence, correctness and usefulness separately

Start with objective checks where the answer key supports them. Verify required
facts, numerical units and tolerances, set membership, dates, cited document
identities and prohibited assertions. Check whether a quotation resolves to its
source. Then review whether that passage actually supports the assertion: a
valid citation identifier or matching quote alone does not establish entailment.

For conflicts, require the material sides of the disagreement and their sources.
For a refusal, check both whether refusing was appropriate and whether the text
nevertheless smuggles in an unsupported answer. For an answerable case, an
unnecessary refusal is a failure too. Otherwise an assistant that refuses
everything can appear deceptively safe.

| Dimension | Useful question to measure |
|---|---|
| Evidence coverage | Did the request receive the evidence needed for this case? |
| Answer correctness | Are the material claims and qualifications supported and accurate? |
| Completeness | Were all required parts or set members included? |
| Citation quality | Do references resolve, support their claims and identify the appropriate versions? |
| Scope and access | Did the response respect the caller, time and document boundaries? |
| Uncertainty handling | Was answering, qualifying, clarifying or declining the appropriate choice? |
| Application usefulness | Can the intended user understand and act on the answer without repairing it? |
| Operating behavior | What were the elapsed time, token usage, cost, errors and retries? |

Use deterministic checks for what they can establish, and human judgment for
meaning that those checks miss. Simple keyword checks can pass an answer that
mentions the right number only to deny it, or fail a correct paraphrase. Keep
them as targeted signals rather than a complete theory of correctness.

If you add a model judge, calibrate it against a reviewed sample containing
both clear and borderline outcomes. Conceal the candidate identity and vary
answer order when comparing alternatives. Retain the judge's version, rubric
and stated rationale, and periodically check disagreements with human reviewers.
A judge score is additional evidence, not independent ground truth merely
because it came from another model.

Report results by question family, document family, caller context and model
tier. Show counts alongside rates. State how many cases failed to execute,
were interrupted or remain ungraded; do not remove them from the report to
improve an average. Define hard acceptance conditions separately from a
weighted quality score so critical failures cannot disappear into the mean.

**Keep:** a scorecard with case-level evidence, repeat-to-repeat variation,
reviewer disagreements and the denominators behind each aggregate.

## 11. Diagnose failures and choose the next improvement

For a failed case, follow its evidence backward: final answer, completion
context, retrieved candidates, indexed chunks, extracted text and original
document. Locate the earliest point where the necessary information was lost
or misinterpreted. That point usually identifies the layer to change next.

Suppose the support assistant gives an obsolete procedure. If the applicable
bulletin never entered the collection, repair the binding. If it was indexed
but lost to many similar tickets, investigate retrieval and ranking. If both
documents reached completion and the answer ignored their applicability, review
the evidence policy and synthesis behavior. These are different experiments.

Maintain a failure log with a compact record for each issue: the case, expected
evidence, observed outcome, suspected mechanism, proposed change and verification
result. Reduce difficult failures to a small diagnostic corpus while preserving
the relationship that caused them. Once fixed, keep the case in the regression
set and retest it in the full corpus, where competing documents can reintroduce
the problem.

Use removal experiments to understand improvements. Temporarily remove a
prompt instruction, ranking adjustment or extra collection and observe whether
its claimed benefit disappears. A setting whose benefit you cannot reproduce
may be unnecessary complexity. Prefer the simplest configuration that meets
the application's measured needs.

**Keep:** a failure history that explains retained changes, rejected changes
and unresolved limitations without replacing old results.

## 12. Challenge the candidate beyond the comfortable cases

Move from the pilot to the full corpus and the actual application flow. Test
long documents, sparse evidence, duplicate passages, near-identical product
names, contradictory revisions and questions that cross collection boundaries.
Include documents whose text contains instructions addressed to an assistant;
verify that untrusted source content remains evidence rather than becoming
application policy.

Exercise restricted callers and fresh sessions as well as realistic follow-up
conversations. Check that a second user cannot inherit the first user's evidence
or conversation state. Test the application's formatting, citation links and
streaming behavior, because correct Server output can still be lost or
misrepresented by the UI.

Measure cold and warm starts separately. Check index availability and known
answers after a Server restart or a controlled redeployment. Exercise provider
timeouts, unavailable evidence and interrupted responses. These conditions
should produce understandable, recorded outcomes rather than apparently complete
answers with missing content.

Repeat representative cases enough to reveal instability. A small repeated
smoke battery is useful for finding defects, but it is not a statistical
guarantee of reliability. Set the repetition policy according to the variation
you observe and the application's consequences. Avoid treating repeated
paraphrases of one task as many independent kinds of success.

**Keep:** an acceptance report on the intended deployment and application path,
with explicit coverage and remaining limitations.

## 13. Release the documents with their evidence

Choose acceptance conditions before looking at the final held-out results.
They should cover material correctness, appropriate refusal, required evidence,
access boundaries and operating limits. Explain any tradeoff: a small quality
gain may not justify a large cost increase, while an important error reduction
may justify slower responses for a particular task.

Identify exactly what you are releasing: shape and runbook versions and hashes,
their resolved references, corpus snapshot, index identities, Server digest,
model configuration and relevant runtime settings. Check the applied documents
against that record. A similarly named file in a working directory is not
evidence of what the deployed session used.

Retain the baseline, candidate, case-set version, rubric and full results beside
the release decision. Name the supported use cases and any exclusions. If an
owner accepts narrower coverage or a known limitation, record that decision
explicitly; unrun cases remain unverified rather than becoming passes.

Apply the tested documents through the supported Server deployment path and
verify their references and index state in the destination. Start with a limited
rollout where practical. Keep the previous working configuration and its required
artifacts available for a compatible rollback. A configuration export is useful,
but it does not replace preserving the data and index versions it refers to.

**Keep:** a reproducible release package and a decision a future maintainer can
understand without interviewing the person who ran the experiments.

## 14. Make improvement a continuing practice

After release, review real failures and user corrections with appropriate
handling of private data. Add representative cases to the development and
regression sets. Check whether the failure changes the answer key, reveals a
new task family or exposes a defect in an existing assumption.

Revisit the laboratory when document composition, extraction, permissions,
models, application behavior or Server versions change. Reuse the baseline
cases, but also introduce genuinely new material. Otherwise the laboratory
eventually measures only its own history.

Stop tuning when the candidate meets the predeclared needs, its improvements
survive validation, and another change has no demonstrated benefit worth its
cost. Prefer shape and runbook documents whose behavior you can explain,
reproduce and maintain on the corpora and use cases your application actually
serves.

## Where to go next

- [Developer guide](dev-guide.md): Server deployment, shapes, runbooks, application
  integration and the worked authoring workflow.
- [Loading corpora](loading-corpora.md): source identity, collection bindings and
  extraction considerations.
- [Retrieval sizing](retrieval-sizing.md): candidate depth, evidence selection
  and completion-context capacity.
- [Evidence hierarchy](evidence-hierarchy.md): evidence roles, authority limits
  and profile validation.
- [Source stores](source-stores.md): where document bytes live and how to preserve
  them independently of a replaceable Server container.
- [Token budgets](../tokenbudgets.md): the limits that must be included in a
  model-backed experiment's cost and completion planning.
