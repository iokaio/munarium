// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

/**
 * One finding from Matrix's own validators. {@code code} is the stable
 * identity a test asserts on; {@code message} is for a human and may be
 * reworded.
 */
public record ValidationFinding(String code, String path, String message) {}
