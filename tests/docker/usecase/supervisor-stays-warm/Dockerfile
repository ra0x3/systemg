FROM sysg-usecase-base

COPY tests/docker/usecase/supervisor-stays-warm/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/supervisor-stays-warm/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
