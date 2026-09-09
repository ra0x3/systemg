FROM sysg-usecase-base

COPY tests/docker/usecase/health-probe-supersede/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/health-probe-supersede/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
