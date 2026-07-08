import gc
import _thread

_serial_marker = "C3_GC_OTHERS_OK"
_worker_objs = None

done = _thread.allocate_lock()
done.acquire()

def worker():
    global _worker_objs
    _worker_objs = [{"id": i, "data": [j * 2 for j in range(10)]} for i in range(50)]
    done.acquire()
    ok = True
    for i in range(50):
        if _worker_objs[i]["id"] != i or _worker_objs[i]["data"] != [j * 2 for j in range(10)]:
            ok = False
            break
    done.release()

_thread.start_new_thread(worker, ())

for _ in range(200000):
    pass

gc.collect()

done.release()
done.acquire()
done.release()
