// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import java.util.List;

/**
 * A queued job. {@code sync} and {@code reconcile} return this rather than an
 * outcome: both can take minutes and must survive the caller hanging up, so
 * they belong to a role's queue and the call returns ids to watch.
 *
 * <p>A sync fans out to one job per authorization class, because a collection
 * carries exactly one class — hence a LIST, not an id.
 */
public record JobAccepted(int accepted, List<String> jobs, String detail) {
    public JobAccepted {
        jobs = jobs == null ? List.of() : List.copyOf(jobs);
    }
}
