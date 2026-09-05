// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/** One row of a registry listing. {@code assetRef} is {@code name@version}. */
public record AssetSummary(
        String assetRef, String name, int version, String kind, String createdAt, String source) {}
