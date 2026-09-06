"""Offline regression tests for the embedded Python API (load current source).

The native module supplies only normal header/session plumbing; loading the source
avoids accidentally testing an older embedded wrapper before rebuilding a wheel.
"""
import asyncio
import importlib.util
from pathlib import Path
import sys
import unittest


def load_wrapper():
    name = 'requests_rs._audit_wrapper'
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parent / 'src/api_wrapper.py')
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


api = load_wrapper()


class Stream:
    def __init__(self):
        self.chunks = [b'first', b'second', b'']
        self.reads = 0
        self.closed = False
        self.close_calls = 0
        self.close_started = None
        self.close_release = None

    def read(self, size=-1):
        self.reads += 1
        return self.chunks.pop(0)

    async def read_async(self, size=-1):
        return self.read(size)

    def close(self):
        self.closed = True

    async def close_async(self):
        self.close_calls += 1
        if self.close_started is not None:
            self.close_started.set()
            await self.close_release.wait()
        self.closed = True


def response(stream, *, asynchronous=False):
    result = api.Response(status_code=200, headers=[], url='http://localhost/',
                          fingerprint_id='test', impersonate='chrome152', stream=stream)
    result._async_stream = asynchronous
    return result


class ResponseTests(unittest.TestCase):
    def test_content_does_not_steal_from_iterator(self):
        stream = Stream()
        result = response(stream)
        iterator = result.iter_content()
        self.assertEqual(next(iterator), b'first')
        with self.assertRaisesRegex(RuntimeError, '活动消费者'):
            _ = result.content
        self.assertEqual(stream.reads, 1)
        self.assertFalse(stream.closed)
        self.assertEqual(next(iterator), b'second')
        iterator.close()
        self.assertTrue(stream.closed)

    def test_sync_iterator_rejects_async_stream_before_reading(self):
        stream = Stream()
        result = response(stream, asynchronous=True)
        with self.assertRaisesRegex(RuntimeError, 'aiter_content'):
            next(result.iter_content())
        self.assertEqual(stream.reads, 0)
        self.assertFalse(stream.closed)
        asyncio.run(result.aclose())

    def test_buffered_iteration_still_works(self):
        result = response(None, asynchronous=True)
        result._content = b'abcdef'
        self.assertEqual(list(result.iter_content(2)), [b'ab', b'cd', b'ef'])
        self.assertEqual(result.content, b'abcdef')


class AsyncResponseTests(unittest.IsolatedAsyncioTestCase):
    async def test_aread_does_not_steal_from_async_iterator(self):
        stream = Stream()
        result = response(stream, asynchronous=True)
        iterator = result.aiter_content()
        self.assertEqual(await anext(iterator), b'first')
        with self.assertRaisesRegex(RuntimeError, '活动消费者'):
            await result.aread()
        self.assertEqual(stream.reads, 1)
        self.assertFalse(stream.closed)
        self.assertEqual(await anext(iterator), b'second')
        await iterator.aclose()
        self.assertTrue(stream.closed)

    async def test_aread_keeps_exclusive_ownership_across_reads(self):
        stream = Stream()
        entered, release = asyncio.Event(), asyncio.Event()
        async def pending_read(size):
            entered.set()
            await release.wait()
            return stream.read(size)
        stream.read_async = pending_read
        result = response(stream, asynchronous=True)
        first = asyncio.create_task(result.aread())
        await entered.wait()
        with self.assertRaisesRegex(RuntimeError, '活动消费者'):
            await result.aread()
        self.assertFalse(stream.closed)
        release.set()
        self.assertEqual(await first, b'firstsecond')
        self.assertTrue(stream.closed)

    async def test_aclose_survives_cancellation_and_is_joined(self):
        stream = Stream()
        stream.close_started, stream.close_release = asyncio.Event(), asyncio.Event()
        result = response(stream, asynchronous=True)
        first = asyncio.create_task(result.aclose())
        await stream.close_started.wait()
        first.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await first
        self.assertFalse(result._close_task.cancelled())
        second = asyncio.create_task(result.aclose())
        await asyncio.sleep(0)
        self.assertFalse(second.done())
        stream.close_release.set()
        await second
        self.assertTrue(stream.closed)
        self.assertEqual(stream.close_calls, 1)

    async def test_async_context_closes_on_error(self):
        stream = Stream()
        result = response(stream, asynchronous=True)
        with self.assertRaisesRegex(ValueError, 'test'):
            async with result:
                raise ValueError('test')
        self.assertTrue(stream.closed)

    async def test_cancelled_aread_releases_consumer_and_stream(self):
        stream = Stream()
        entered = asyncio.Event()
        async def pending_read(size):
            entered.set()
            await asyncio.Event().wait()
        stream.read_async = pending_read
        result = response(stream, asynchronous=True)
        task = asyncio.create_task(result.aread())
        await entered.wait()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertFalse(result._consumer_active)
        self.assertTrue(stream.closed)


class CookieAndInputTests(unittest.TestCase):
    def test_cookie_set_cannot_expand_scope_via_value_or_attributes(self):
        with api.Session(impersonate='chrome152') as session:
            for kwargs in (
                {'value': 'ok; Domain=example.com'},
                {'value': 'ok', 'path': '/; Domain=example.com'},
                {'value': 'ok', 'domain': 'example.com; Path=/'},
                {'value': 'ok\r\nInjected: yes'},
                {'value': 'ok\x00'},
            ):
                with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                    session.cookies.set('a', url='https://sub.example.com', **kwargs)
                self.assertEqual(session.cookies.get_all(), [])
            session.cookies.set('a', 'ok=1', url='https://sub.example.com', secure=True)
            self.assertEqual(session.cookies['a'], 'ok=1')
            self.assertTrue(session.cookies.get_all()[0].secure)

    def test_request_cookie_pairs_cannot_inject_additional_cookie(self):
        for cookies in ({'a': '1; admin=true'}, [('a', '1; admin=true')], {'a=b': '1'}, {'': '1'}):
            with self.subTest(cookies=cookies), self.assertRaises(ValueError):
                api._cookie_items(cookies, 'https://example.com')
        self.assertEqual(api._cookie_items({'a': 'x=y', 'b': ''}, 'https://example.com'),
                         [('a', 'x=y'), ('b', '')])

    def test_timeout_does_not_treat_bool_as_seconds(self):
        with self.assertRaises(ValueError):
            api.Session(impersonate='chrome152', timeout=True)

    def test_redirect_limit_requires_integer(self):
        with api.Session(impersonate='chrome152') as session:
            for limit in (True, 1.5, '3', -1):
                with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, 'max_redirects'):
                    session.get('http://127.0.0.1:1/', max_redirects=limit)

    def test_websocket_protocol_string_not_silently_split_into_characters(self):
        with api.Session(impersonate='chrome152') as session:
            for protocols in ('chat', b'chat', [123]):
                with self.subTest(protocols=protocols), self.assertRaisesRegex(TypeError, 'protocols'):
                    session.websocket('ws://127.0.0.1:1/', protocols=protocols)


if __name__ == '__main__':
    unittest.main()
