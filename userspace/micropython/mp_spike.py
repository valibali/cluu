import sys
import gc

count = 0
for i in range(100):
    buf = [j * j for j in range(100)]
    count += 1
    del buf
    gc.collect()

print("MP_SPIKE_OK " + str(count))

try:
    f = open("/etc/passwd")
    f.close()
except Exception:
    print("MP_NO_VFS_OK")
