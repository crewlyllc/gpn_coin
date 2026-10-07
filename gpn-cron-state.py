import json
d=json.load(open('/home/jeremyg/.openclaw/cron/jobs.json'))
jobs=d if isinstance(d,list) else d.get('jobs',d)
for j in jobs:
    if j.get('id') in ('1a4f19ab-c029-49db-ad89-69167dff2b22','95b27deb-30af-4e79-994a-f8eb451276f3'):
        s=j.get('state',{})
        print(j['id'][:8], j.get('name'))
        print('   lastRunStatus=', s.get('lastRunStatus'), 'lastDeliveryStatus=', s.get('lastDeliveryStatus'), 'lastDelivered=', s.get('lastDelivered'))
