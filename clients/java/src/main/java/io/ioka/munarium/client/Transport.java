// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client;

import io.ioka.munarium.client.errors.UnsupportedTransportException;
import io.ioka.munarium.client.model.Meta;
import io.ioka.munarium.client.planes.Planes;

/**
 * One transport = all ten planes + the meta route + a close. Both transports
 * implement this, so the facade needs no casts and a transport missing a
 * plane is a COMPILE error, not a construction-time ClassCastException; a
 * decorating transport (metrics, logging) composes naturally.
 */
public interface Transport
        extends Planes.CommandsPlane,
                Planes.QueryPlane,
                Planes.IngestPlane,
                Planes.RetrievalPlane,
                Planes.RunbooksPlane,
                Planes.ProvidersPlane,
                Planes.SessionsPlane,
                Planes.AccessTokensPlane,
                Planes.ReportsPlane,
                Planes.AuthoringPlane,
                Planes.EvidencePlane,
                AutoCloseable {

    /**
     * {@code GET /version} — REST meta; the gRPC transport keeps the
     * default: the same typed refusal every other REST-only surface uses.
     */
    default Meta.ServerVersion serverVersion() {
        throw new UnsupportedTransportException(
                "GET /version is a REST meta route — use the REST client, or gRPC server"
                        + " reflection");
    }

    @Override
    void close();
}
