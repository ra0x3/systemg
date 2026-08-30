FROM sysg-usecase-base

COPY tests/docker/usecase/health-unhealthy-cascades/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/health-unhealthy-cascades/probe.py /usecase/probe.py
COPY tests/docker/usecase/health-unhealthy-cascades/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
