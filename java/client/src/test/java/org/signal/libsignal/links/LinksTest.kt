//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

package org.signal.libsignal.links

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.io.File

/**
 * Replays `rust/links/tests/data/bridge-golden.json` through the JNI bridge and compares every
 * result byte for byte with what Rust produced (the same file is replayed by the Swift and
 * TypeScript tests).
 */
class LinksTest {
  private val data = File("../../rust/links/tests/data")
  private val golden = Json.parseToJsonElement(File(data, "bridge-golden.json").readText()).jsonObject

  private fun JsonObject.str(key: String): String = this[key]!!.jsonPrimitive.content

  private fun JsonObject.strOrNull(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull

  private fun registry(): LinkRegistry = LinkRegistry.load(File(data, golden.str("registry")).readBytes())

  @Test
  fun loadsTheShippedRegistry() {
    val registry = registry()
    assertEquals(golden["registry_version"]!!.jsonPrimitive.long, registry.version)
    assertEquals(golden.str("degraded"), registry.degraded())
  }

  @Test
  fun classifyAndReceiveCheck() {
    val registry = registry()
    for (case in golden["classify"]!!.jsonArray.map { it.jsonObject }) {
      val name = case.str("name")
      assertEquals(
        name,
        case.str("card"),
        registry.classify(case.str("preview"), case.str("body"), case.str("message")),
      )
      assertEquals(
        name,
        case.str("receive_check"),
        registry.receiveCheck(case.str("preview"), case.str("body"), case.str("message")),
      )
    }
  }

  @Test
  fun openPlanAndIdentify() {
    val registry = registry()
    for (case in golden["open_plan"]!!.jsonArray.map { it.jsonObject }) {
      assertEquals(case.str("url"), case.str("plan"), registry.openPlan(case.str("url")))
    }
    for (case in golden["identify"]!!.jsonArray.map { it.jsonObject }) {
      val location = case["location"]!!.jsonPrimitive.boolean
      assertEquals(case.str("url"), case.strOrNull("result"), registry.identify(case.str("url"), location))
    }
  }

  @Test
  fun layoutAndTint() {
    for (case in golden["layout"]!!.jsonArray.map { it.jsonObject }) {
      val layout =
        Links.layout(
          case["width"]!!.jsonPrimitive.int,
          case["height"]!!.jsonPrimitive.int,
          case.str("kind"),
          case.str("level"),
        )
      assertEquals(case.str("layout"), layout)
    }
    for (case in golden["tint"]!!.jsonArray.map { it.jsonObject }) {
      val rgba =
        case
          .str("rgba_hex")
          .chunked(2)
          .map { it.toInt(16).toByte() }
          .toByteArray()
      val tint =
        Links.tint(
          case.str("layout"),
          case["width"]!!.jsonPrimitive.int,
          case["height"]!!.jsonPrimitive.int,
          rgba,
        )
      assertEquals(case.str("tint"), tint)
    }
  }

  @Test
  fun sendEveryRequestAndTheOutcome() {
    val registry = registry()
    for (case in golden["send"]!!.jsonArray.map { it.jsonObject }) {
      val script = case["script"]!!.jsonObject
      val job = registry.begin(case.str("url"), case.str("context"))
      val requests = mutableListOf<String>()
      while (true) {
        val request = job.nextRequest() ?: break
        requests.add(request)
        val parsed = Json.parseToJsonElement(request).jsonObject
        val id = parsed["id"]!!.jsonPrimitive.int
        when (parsed.str("type")) {
          "first_party" -> job.onFirstParty(id, script.strOrNull("first_party") ?: "{}")
          "image" -> job.onImage(id, (script["image_ok"] as? JsonPrimitive)?.booleanOrNull ?: false)
          else -> {
            val url = parsed.str("url")
            val reply = (script["responses"] as? JsonObject)?.get(url)?.jsonObject
            val networkErrors = (script["network_error"] as? JsonArray)?.map { it.jsonPrimitive.content } ?: emptyList()
            when {
              reply != null ->
                job.onResponse(
                  id,
                  reply["status"]!!.jsonPrimitive.int,
                  reply.str("final_url"),
                  reply.str("content_type"),
                  reply.strOrNull("location"),
                  reply.str("body").toByteArray(Charsets.UTF_8),
                )
              url in networkErrors -> job.onNetworkError(id)
              else -> job.onFailure(id)
            }
          }
        }
      }
      val expected = case["requests"]!!.jsonArray.map { it.jsonPrimitive.content }
      assertEquals(case.str("name"), expected, requests)
      assertEquals(case.str("name"), case.str("outcome"), job.finish())
    }
  }

  @Test
  fun badJsonIsAnErrorNotACrash() {
    val registry = registry()
    assertThrows(IllegalArgumentException::class.java) { registry.classify("{", "", "{}") }
    assertThrows(IllegalArgumentException::class.java) { Links.layout(1, 1, "", "nope") }
  }
}
