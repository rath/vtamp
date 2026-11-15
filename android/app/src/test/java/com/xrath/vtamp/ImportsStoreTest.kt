package com.xrath.vtamp

import com.xrath.vtamp.api.*
import com.xrath.vtamp.model.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test

class ImportsStoreTest {
    private fun job(id: Int, status: String = "completed", added: Int = 0, revision: Int = 1) =
        ImportJob(
            "j$id",
            "https://example.com/$id",
            "Import $id",
            status,
            added = added,
            startedAtMs = id.toLong(),
            revision = revision.toLong(),
        )

    private fun reply(page: ImportsPage) =
        MockResponse()
            .setBody("""{"version":15,"ok":true,"data":${wireJson.encodeToString(page)}}""")

    @Test
    fun loadingOlderHistoryAndPollingKeepTheRevealedTail() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(reply(ImportsPage((40 downTo 21).map { job(it) }, "older")))
            server.enqueue(reply(ImportsPage((20 downTo 1).map { job(it) })))
            server.enqueue(reply(ImportsPage((41 downTo 22).map { job(it) }, "different")))
            val store = ImportsStore(this) {}
            store.configure(VtampClient(server.url("/api")), true)
            store.refresh()
            store.loadMore()
            withTimeout(3000) { store.state.first { it.jobs.size == 40 } }
            store.refresh()
            assertEquals(41, store.state.value.jobs.size)
            assertFalse(store.state.value.more)
        }
    }

    @Test
    fun failedPageKeepsItsCursorForRetry() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(reply(ImportsPage(listOf(job(2)), "older")))
            server.enqueue(MockResponse().setResponseCode(500).setBody("failed"))
            server.enqueue(reply(ImportsPage(listOf(job(1)))))
            val store = ImportsStore(this) {}
            store.configure(VtampClient(server.url("/api")), true)
            store.refresh()
            store.loadMore()
            withTimeout(3000) { store.state.first { it.error != null } }
            assertTrue(store.state.value.more)
            store.loadMore()
            withTimeout(3000) { store.state.first { it.jobs.size == 2 } }
            server.takeRequest()
            val failed = server.takeRequest().body.readUtf8()
            val retried = server.takeRequest().body.readUtf8()
            assertEquals(failed, retried)
        }
    }

    @Test
    fun completionOutsideVisiblePageRefreshesLibrary() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(reply(ImportsPage(listOf(job(40)), "old", listOf(job(1, "running")))))
            server.enqueue(reply(ImportsPage(listOf(job(40)), "old")))
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"version":15,"ok":true,"data":{"job":${wireJson.encodeToString(job(1, added = 1, revision = 2))}}}"""
                    )
            )
            var changes = 0
            val store = ImportsStore(this) { changes++ }
            store.configure(VtampClient(server.url("/api")), true)
            store.refresh()
            store.refresh()
            assertEquals(1, changes)
            assertEquals(1, store.state.value.jobs.size)
        }
    }

    @Test
    fun legacyServerPagesLocally() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setBody(
                        """{"version":15,"ok":true,"data":${wireJson.encodeToString((1..45).map { job(it) })}}"""
                    )
            )
            val store = ImportsStore(this) {}
            store.configure(VtampClient(server.url("/api")), false)
            store.refresh()
            assertEquals(20, store.state.value.jobs.size)
            store.loadMore()
            assertEquals(40, store.state.value.jobs.size)
            store.loadMore()
            assertEquals(45, store.state.value.jobs.size)
            assertFalse(store.state.value.more)
            assertEquals(1, server.requestCount)
        }
    }

    @Test
    fun changingServersDiscardsDelayedReply() = runBlocking {
        MockWebServer().use { server ->
            val arrived = CountDownLatch(1)
            val release = CountDownLatch(1)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        arrived.countDown()
                        release.await(3, TimeUnit.SECONDS)
                        return reply(ImportsPage(listOf(job(1))))
                    }
                }
            val store = ImportsStore(this) {}
            store.configure(VtampClient(server.url("/api")), true)
            val request = launch { store.refresh() }
            withContext(Dispatchers.IO) { assertTrue(arrived.await(3, TimeUnit.SECONDS)) }
            store.configure(null, false)
            release.countDown()
            request.join()
            assertTrue(store.state.value.jobs.isEmpty())
        }
    }
}
